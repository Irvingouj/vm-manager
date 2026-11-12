# OpenWorkers as an execution core: what the host layer must be

Research date: 2026-11-11. Method: source reading of shallow clones (full history for `openworkers-core` and `openworkers-runtime-v8`), plus `gh api` for metadata. Nothing was built or run; no Rust toolchain was used.

Commits read:

| Repo | Commit | Commit date | Version in Cargo.toml |
|---|---|---|---|
| openworkers/openworkers-core | `0bdc4e4e687eee2decb8b8d56403ac63921911bb` | 2026-10-30 | 0.18.1 |
| openworkers/openworkers-runtime-v8 | `4ea28c1c26ea10920ad71b8e7bec264f735f7e6e` | 2026-11-01 | 0.18.7 |
| openworkers/openworkers-runner | `abadd24700f9a3bb00d382103b5129eb98a96d6a` | 2026-11-01 | 1.18.8 |
| openworkers/openworkers-embed | `ab01f30f2e34a28f08a852e53359fda897b08a73` | 2026-11-07 | 0.18.4 |
| openworkers/postgate | `4c3ab14d40eb8bdd3012c16d95efe7c43c8d2530` | 2026-10-10 | 0.1.8 |
| openworkers/openworkers-infra | `317d2b23e5931799722b036423d0e1f446d810d4` | 2026-10-14 | (no Cargo) |
| openworkers/openworkers-transform | `8e0a49f92507f1f9791768ffe39c0b32c625af20` | (HEAD) | 0.1.4 |
| openworkers/openworkers-wintertc | `3ef4512820d52f3160d1d02006fae5d681dfa1b6` | (HEAD) | (JS surface) |
| openworkers/rusty-v8 | `1e84b5f26` (metadata only, not cloned) | 2026-09-18 | 152.2.0 |

Paths below are `repo/path:line`; repo prefixes are shortened to `core`, `rt` (runtime-v8), `runner`, `embed`, `postgate`, `transform`, `wintertc`. Line counts are `wc -l` on `.rs` files and include inline unit tests and comments.

## Answer

**The source favours (b): our own host built directly on `openworkers-core` + `openworkers-runtime-v8`, dropping the runner.** Keep (c), a fork, as the fallback. Don't take (a).

The facts that decide it:

1. **The core/runtime boundary is clean and an outside host already uses it.** Every outward call from guest JS becomes an `Operation` that goes to one `OperationsHandler` trait object, and every method on that trait has a default stub (read in source, `core/src/ops/mod.rs:142-238`, `:304-551`). `openworkers-embed` is a working host of about 3,000 lines. It builds its own warm pool from the public `LockerManagedIsolate` + `ExecutionContext::new_with_pooled_isolate` / `reset` / `begin_request` / `exec` / `drain_waituntil` API, with no runner code (read in source, `embed/src/engine/warm.rs:190-255`).
2. **The runner is a product, not a library you can patch thinly.**
   - Only one runner can run per platform database: a Postgres advisory lock enforces it (read in source, `runner/src/singleton.rs:1-30`, `runner/bin/main.rs:1242-1248`).
   - It reads OpenWorkers' own schema: `workers`, `worker_deployments`, `environments`, `projects`, `domains`, `crons`, `*_configs`, `resolve_worker_from_request` (read in source, SQL in `runner/src/store.rs`, `runner/src/resolve_cache.rs`).
   - Guest SQL in "platform" mode runs on a pool opened with the **same `DATABASE_URL`, and so the same role, as the runner's internal pool**. Only `SET LOCAL search_path` confines it (read in source, `runner/bin/main.rs:1148-1161`, `:1238-1240`; `runner/src/services/database.rs:147-200`).
3. **The runtime carries no request identity, so the host must supply it.**
   - An operation carries only the binding name and its arguments (`core/src/ops/mod.rs:142-238`).
   - Attribution works only because each V8 context gets exactly one handler object, fixed when the context is created and moved into the context's event loop (`rt/src/execution_context.rs:121-284`).
   - A cached context is taken out of the cache while a request uses it, so one context never serves two requests at once (`rt/src/pool.rs:573`).
   - The runner's pattern for per-request state is a `Mutex<RequestState>` that `update_request` swaps on a warm hit (`runner/src/ops.rs:136-158`, `:266-272`; `runner/src/task_executor.rs:250-260`).
   - Whoever owns the host owns this slot. In (b) we write it once, in our own handler.
4. **We don't need to touch the runtime to add our SQL operation.** The existing `database` binding type already exposes `env.<NAME>.query(sql, params)` to the guest and delivers `Operation::BindingDatabase { binding, op }` to the handler. The `invoke` feature gives a generic `env.<NAME>.invoke(command, jsonArgs)` (`rt/src/js/env.js:243-265`). The runner doesn't enable `invoke`; only embed does (read in source, `runner/Cargo.toml:69,75`; `embed/Cargo.toml` deps).

**Biggest risk of (b): one maintainer and a pre-1.0 API that moves fast.**
- runtime-v8 has 112 tags. It went from 0.15.0 (2026-08-21) to 0.18.7 (2026-11-01). 0.15.0 to 0.16.0 deleted 6,545 lines and changed 47 `pub` items (from git history).
- The only human contributor on runner and runtime-v8 is `max-lt` (from metadata).
- The engine comes through three crates the same person forked and publishes: `openworkers-v8` (rusty_v8), `openworkers-serde-v8` and `openworkers-glue-v8`.

Adopting (b) means pinning tags and budgeting for doing the V8 bumps ourselves when upstream stops.

## 1. The execution core

**Smallest set.** The minimum is `openworkers-core` (types and the `OperationsHandler` trait) plus `openworkers-runtime-v8`. runtime-v8 brings these git and crates.io dependencies with it (read in source, `rt/Cargo.toml:20-52`):

| Dependency | Role |
|---|---|
| `openworkers-wintertc` (tag v0.1.11) | The Web API surface, as JavaScript |
| `openworkers-v8` 152.2.0 | Fork of rusty_v8 |
| `openworkers-serde-v8` 152.2.2 | Fork of serde_v8 |
| `openworkers-glue-v8` 152 | Fork of glue_v8 |
| `gc-derive` | In-tree macro crate |

The guest bundle is a classic script, not an ES module (point 3 below). So you also need `openworkers-transform` to rewrite `export default` to `globalThis.default`, or a bundler that emits that shape. The runner pulls it in through its `_js` feature (`runner/Cargo.toml:21-22,71-72`).

**Two ways to drive it** (read in source):
- **`Worker`**: one isolate per worker, with `Worker::new_with_ops(script, limits, ops)` then `exec(event)` (`rt/src/worker.rs:100-158`).
- **`execute_pinned`**: the thread-local pool with warm contexts (`rt/src/pool.rs:508-822`). It is configured **once per process** through `init_pinned_pool(PinnedPoolConfig)`, and one `RuntimeLimits` applies to every isolate (`rt/src/pool.rs:89-117`; `rt/src/pool_common.rs:7-26`). Per-application limits therefore need `Worker`, or our own pool on `ExecutionContext` as embed builds one (`embed/src/engine/warm.rs:190-214`).

Limits:
- **CPU, wall clock, heap** come from `RuntimeLimits` (`core/src/limits.rs:36-79`) and are enforced inside the runtime (Q6).
- **Per-binding call counts** (`fetch_limit`, `database_limit`, ...) are not enforced by the runtime. The runner enforces them in its own `BindingLimiters` (`runner/src/limiter.rs:183-200`).

**Sizes** (lines of Rust unless marked; read in source):

| Part | Lines | Notes |
|---|---|---|
| core `src/` | 3,183 | of which ops/D1 201, ops/durable_object 110, ops/images 195, ops/sql 493 |
| runtime-v8 `src/` | 16,818 | + 993 JS in `src/js/`; Durable Object support ≈2,019 (rs+js); crypto 1,695; bindings 3,110; scheduler 1,255; execution_context 1,591; pool 1,062 |
| wintertc | 4,369 JS + 265 Rust | Web API surface, snapshotted |
| transform | 613 | SWC pass |
| embed `src/` | 3,066 | a complete alternative host |
| runner `src/` + `bin/` | 20,578 | breakdown below |
| postgate `src/` | 1,880 | the runner uses only its parser |

The runner's 20,578 lines break down as follows (read in source). Each part is something we would drop or rebuild:

| Part | Lines | Drop or rebuild |
|---|---|---|
| Ingress / TLS / allowlist (`ingress.rs`) | 1,178 | rebuild (ours) |
| Process main, routing, admin (`bin/main.rs`) | 1,546 | rebuild |
| Worker pool, permits, executor (`worker_pool.rs`, `task_executor.rs`, `worker.rs`, `runtime.rs`) | 1,672 | rebuild thin |
| `RunnerOperations` (`ops.rs`) | 1,299 | rebuild (this is "our handler") |
| Fetch, S3 storage, KV, DB, SSRF guard (`services/`) | 2,472 | keep the ideas; `net_guard.rs` (288) is worth copying |
| Platform DB access and resolution (`store.rs`, `resolve_cache.rs`) | 1,757 | drop (OpenWorkers schema) |
| D1 (SQLite) (`d1/`) | 4,214 (2,583 non-test files) | drop |
| Durable Objects (`durable/`) | 2,720 (1,702 non-test files) | drop |
| Logs (`logs.rs`, `log.rs`) | 919 | rebuild into our audit pipeline |
| Crons (`scheduler.rs`, `event_scheduled.rs`) | 446 | drop or rebuild |
| Inbound websockets (`websocket.rs`) | 487 | optional |
| Metrics / telemetry / limiter / code cache / wasm cache / misc | ≈1,800 | mostly drop; `limiter.rs` (342) is a useful pattern |

## 2. Attributing an operation to a request

**Trace of one call, `await env.DB.query("select 1", [])`** (read in source):

1. **The guest-side wrapper.** `globalThis.env` is built once per context by `js/env.js`, from JSON describing the binding names and types (`rt/src/worker.rs:275-310`). `env.DB.query` calls `__bindingCall(__nativeBindingDatabase, "DB", "query", {sql, params})` (`rt/src/js/env.js:243-246`; `__bindingCall` at `rt/src/runtime/bindings/web_api.rs:243`).
2. **The native function.** `__nativeBindingDatabase` is a V8 function registered on the context's global (`rt/src/runtime/bindings/fetch.rs:380-433`).
   - It reads the binding name **as a string from the guest**.
   - It parses `DatabaseParams` and stores the resolve/reject functions under a fresh `callback_id`.
   - It sends `SchedulerMessage::BindingDatabase(callback_id, binding, op)` on the context's channel.
   - Nothing in that message identifies the request.
3. **The event loop.** The context's event loop (`rt/src/runtime/scheduler.rs:189-205`) spawns a task that calls `ops.handle(Operation::BindingDatabase { binding, op })`, racing it against the per-request cancellation token (`rt/src/runtime/scheduler.rs:383-410`). `ops` is the single `OperationsHandle` moved into the loop when the context was created (`rt/src/execution_context.rs:274-284`).
4. **The trait's default dispatch.** `OperationsHandler::handle` calls `handle_binding_database(&binding, op)` (`core/src/ops/mod.rs:483-500`). Our implementation goes here.
5. **The way back.** The result comes back as `CallbackMessage::DatabaseResult` and settles the promise (`rt/src/runtime/dispatch.rs:188-199`).

**So the host knows the request only through `self`.** The handler object is per context. In the runner it is a `RunnerOperations` built per request on the cold path (`runner/src/task_executor.rs:96-125`). On a warm hit the request's new handler is **discarded**: the cached one is kept, and the `on_warm_hit` callback updates it (`rt/src/pool.rs:614-647`; `rt/src/pool_common.rs:28-32,63-69`). The runner downcasts through `as_any` and calls `update_request(log_tx, span, depth)`, which swaps a `Mutex<RequestState>` (`runner/src/task_executor.rs:250-260`; `runner/src/ops.rs:136-158,266-272`). The `invoke` path is the same: it clones `state.ops`, which was captured at context creation (`rt/src/runtime/bindings/fetch.rs:668-716`).

**`RunnerOperations.user_id` is the tenant, and on ordinary fetches it is not even set.**
- `task_components` builds the handler with `worker_id`, bindings, logs, span and depth, but never calls `with_user_id` (`runner/src/task_executor.rs:102-110`). Only the Durable Object path sets it (`runner/src/durable/mod.rs:673`).
- The tenant is used as the pool's `owner_id`: isolates are shared by all workers of one tenant (`runner/src/task_executor.rs:221-223`).
- There is no end-user identity anywhere in the runner (read in source).

**Pinned pool and warm contexts** (read in source):
- **Lookup.** Contexts are cached per isolate under the key (worker_id, version, env_updated_at).
- **Exclusive use.** A hit is removed from the cache with `swap_remove` and put back only after the request and all its `waitUntil` work finish (`rt/src/pool.rs:555-573`, `:656-706`).
- **No in-flight work at reuse.** A context is cached again only if `drain_waituntil` succeeds, so no op of the previous request is still running when the next request swaps the handler's state.
- **Reuse cap.** A context is reused up to `max_context_reuses` times (runner default 1000, `CONTEXT_MAX_REUSES`; `runner/bin/main.rs:1333-1336`).
- **Turning reuse off.** `max_cached_contexts: 0` makes `keep_context` false (`rt/src/pool_policy.rs:168-170`).

**More than one request per isolate (`ISOLATE_MAX_CONCURRENT` > 1).**
- The default in code is 1 (`runner/bin/main.rs:1326-1331`), but the runner's Docker image sets `ISOLATE_MAX_CONCURRENT=20` (`runner/Dockerfile`, last lines).
- With more than one, requests share the isolate through the `AsyncWaiter` fair queue (`rt/src/pool.rs:24-75`).
- Each concurrent request still has its own `ExecutionContext`, because a cached context is exclusive. A second concurrent request to the same worker takes the cold path and builds a new context with its own handler (`rt/src/pool.rs:723-741`).
- So handler state is never shared between in-flight requests.
- What they do share is the **isolate**: one V8 heap limit and one ArrayBuffer budget across all of the owner's requests (`rt/src/pool_common.rs:40-45`).
- The CPU and wall-clock guards are keyed by request id and stop only the request that holds the isolate's turn (`rt/src/execution_context.rs:758-772`; `rt/src/security/timeout_guard.rs:1-17`).

**Can a guest carry something from one request into another?** (read in source unless marked)

| What carries over | Status |
|---|---|
| Timers and intervals | Cleared by a function built before guest code runs, so the guest can't replace it (`rt/src/execution_context.rs:1435-1473`, called in `reset` `:1537-1575`). |
| Pending callbacks, stream state, `AsyncContext` | Cleared. The callback id counter keeps counting, so a late answer finds no callback (`:1566-1572`). |
| In-flight ops of the previous request | `BeginRequest` cancels them and resets the cancellation scope (`rt/src/runtime/scheduler.rs:624-640`). |
| Module-level JS state | **Survives**, by design of warm reuse. A guest can stash user A's data (headers, cookies, query results) in a global and use it while serving user B. |
| Stored promise resolvers or closures from A | A guest can resume them during B; that continuation's ops then run under B's handler state (inferred from code, not verified). |
| Forging another request's identity | Not possible, because the guest has no handle on it. Identity lives only in host state that the guest cannot reach. |
| Native-call globals (`__nativeBindingDatabase`, `__bindingCall`, ...) | Writable plain globals, set with `global.set` (`rt/src/runtime/bindings/macros.rs:12-17`). Only the `__ow` namespace is frozen (`rt/src/runtime/bindings/native.rs:14-41`). A guest can therefore wrap its own native calls, but only inside its own context. |
| Binding name | A free string. The handler must check it against the application's configuration, as the runner does (`runner/src/ops.rs:642-652`). |

For our model this means three things:
- Per-request identity is sound if our handler holds a "current request" slot that is set before `exec` and cleared after `drain_waituntil`.
- Ops issued while request B runs are attributed to B.
- Cross-user leakage *inside one application's own JS* cannot be prevented by the host, unless warm reuse is turned off (a fresh context per request).

## 3. Adding our own operation family

**Option A: reuse what exists, with no runtime change.**
- **`database` binding** (`BindingType::Database`, `core/src/script.rs:97-117`). The guest gets `env.<NAME>.query(sql, params)` (`rt/src/js/env.js:243-246`) and the handler receives `handle_binding_database(binding, DatabaseOp::Query { sql, params })` (`core/src/ops/mod.rs:352-359`; `core/src/ops/sql.rs:263-271`). With the binding name equal to the Resource name, this is already "execute this SQL against the Resource named X". Results can be `Rows(json)` or a typed `Table` (`core/src/ops/sql.rs:280-290`).
- **`invoke` binding** (core feature `invoke`, `core/Cargo.toml:18-19`; `core/src/ops/mod.rs:202-211,399-408`). The guest calls `env.<NAME>.invoke(command, args)` with arbitrary JSON in and out (`rt/src/js/env.js:257-265`). runtime-v8 polls the host future inline up to 16 times per event-loop round before falling back to the scheduler (`rt/src/runtime/bindings/fetch.rs:656-716`). This is the most flexible route for an SDK.
- **Where `invoke` is wired.** Only in embed: `features = ["invoke"]` on both core and runtime-v8 (`embed/Cargo.toml`, dependency lines). The runner enables `ptrcomp` only, and its sources contain no `invoke` (`runner/Cargo.toml:69,75`; grep of `runner/src`, `runner/bin`).

**Option B: a new `Operation` variant.** Adding a family the way `database` was added touches the following places (read in source, by following `BindingDatabase`):
- core: `BindingType` (`core/src/script.rs:97`), `Operation` and `OperationResult` (`core/src/ops/mod.rs:142-280`), the trait method and the `handle` dispatch (`:304-551`).
- runtime-v8:
  - the native function (`rt/src/runtime/bindings/fetch.rs`)
  - a `SchedulerMessage` and a `CallbackMessage` variant, plus the scheduler arm (`rt/src/runtime/scheduler.rs:92,138,383-410`)
  - promise settling (`rt/src/runtime/dispatch.rs:188-199`, `rt/src/runtime/callback_handlers.rs:356-390`)
  - the `make[type]` entry in `rt/src/js/env.js:202-273`
  - the type-name mapping (`rt/src/worker.rs:289-300`)

`Operation` is not `#[non_exhaustive]`, and every backend must name the same core tag (`runner/Cargo.toml:66-69`). A new variant is therefore a breaking change for every runtime crate. Option A avoids all of it.

**How the JS side is injected** (read in source):
- **Snapshot.** The wintertc Web API surface (and `setup_fetch_helpers`) is baked into a V8 startup snapshot (`rt/src/snapshot.rs:35-52`).
- **Native functions** are registered per context and are never in the snapshot (`rt/src/execution_context.rs:164-198`).
- **`env.js`** is evaluated per context with the binding list (`rt/src/worker.rs:275-310`).

So a new native op or a change to `env.js` needs no snapshot regeneration. Only changes to the surface JS do (`rt/CLAUDE.md`, "After editing").

The snapshot file is read at runtime from an absolute path baked in at compile time (`RUNTIME_SNAPSHOT_PATH`). If the file is missing or empty, the runtime falls back to evaluating the surface, which makes startup slower (`rt/src/platform.rs:51-88`; `rt/.cargo/config.toml`).

## 4. A hook before the worker runs

These locations are in the runner (read in source):

- **`public_request`.** All public traffic passes here: `ingress::prepare` strips runner-only headers (`x-worker-id`, `x-forwarded-*`, ... `runner/src/ingress.rs:27-42,448-527`), then the request is routed (`runner/bin/main.rs:202-227`).
- **The internal listener skips `prepare`.** It calls `handle_request` directly (`runner/bin/main.rs:240-254`). It is the path for worker-to-worker calls and trusts `x-worker-*`. Any session design has to treat it separately.
- **`handle_worker_request`, the natural hook.** The worker is resolved by `main.rs:609` (span records `user_id`), so the Application is known there.
  - The `HttpRequest` that the guest sees is built from the `headers` copy at `main.rs:716-790` (`from_hyper_parts[_streaming]`).
  - Resolving a session there and removing `cookie` or `authorization` from that copy before line 716 means the guest never sees them.
  - The resolved identity then has to be threaded through `run_fetch` (`main.rs:890`) → `TaskExecutionConfig` (`runner/src/event_fetch.rs:48-56`) → `task_components` (cold) and the `on_warm_hit` closure → `update_request` (warm) (`runner/src/task_executor.rs:96-125,245-260`).

That is five places across four files in a codebase we don't control, which argues for (b). In our own host the hook is simply our HTTP handler: authenticate, strip headers, set the handler's request slot, then call `exec`.

## 5. Database path

How the runner runs a guest statement (read in source):

1. **Validation.** `handle_binding_database` checks that the binding is configured, takes a permit from the per-request DB limiter (default 100 total, 5 concurrent: `core/src/limits.rs:73-76`), then calls `postgate::parse_and_validate(sql, &HashSet::new())`, meaning "all operations allowed" (`runner/src/ops.rs:634-700`).
2. **postgate as a library is only a parser** (`postgate/src/parser.rs:49-184`):
   - It rejects multiple statements and empty queries.
   - It allows only `Query`, `Insert`, `Update`, `Delete`, `CREATE TABLE/INDEX/VIEW`, `ALTER TABLE/INDEX`, `DROP` and `TRUNCATE`. Everything else, including `SET`, `BEGIN` and `SET ROLE`, is `UnsupportedStatement`.
   - It rejects schema-qualified relations, except `postgate_helpers.*`, and relations named `pg_*` or `information_schema`.
   - It does **not** look at function calls.
   - The runner doesn't use postgate's executor, row limit or timeout (`postgate/src/executor.rs:60-190` is unused by the runner).
3. **Execution** (`runner/src/services/database.rs`) depends on the provider:
   - **`postgres` provider (connection string).** A per-connection-string LRU of sqlx pools (256 pools, 4 connections each). The host is checked to be public and Unix sockets are refused. The statement runs **without an explicit transaction**, as whatever role the guest's connection string names (`:58-135`).
   - **`platform` provider (schema).** It uses the runner's `db_worker` pool. That pool is built from the same `DATABASE_URL` as the runner's internal pool (`runner/bin/main.rs:1148-1161,1238-1240`), so the same role. Each statement runs in its own transaction: `BEGIN; SET LOCAL search_path TO "<schema>"; <wrapped statement>; COMMIT` (`:147-200`). SELECTs and `RETURNING` mutations are wrapped as `jsonb_agg(row_to_json(t))` (`:202-225`).
4. **Transactions across statements.** Not possible. `DatabaseOp` has only `Query` (`core/src/ops/sql.rs:263-271`), each op is its own transaction or autocommit, and `BEGIN` is rejected by the parser.
5. **Limits.** These apply: the binding limiter's count and concurrency, a 5 s pool acquire timeout, and the request's wall clock. These don't: no statement timeout and no row or result-size limit on this path (grep for `statement_timeout` or a row limit in `runner/src`: none).

**Can the host set per-statement session variables that guest SQL can't overwrite?** Not as it stands.
- The host could add `SET LOCAL app.user_id = ...` inside the same transaction.
- But postgate passes any `SELECT` whatever functions it calls. So a guest can call `set_config('app.user_id', ..., true)`, or `set_config('search_path', ..., true)`, inside its own statement (read in source for postgate; the Postgres behaviour of `set_config` and of the `role` setting is not verified here).

Making identity tamper-proof would take one of these:
- function-level validation (deny `set_config` and similar)
- or not giving guests raw SQL at all: host-side query building, or `SECURITY DEFINER` functions that read identity from a source the guest can't write
- or a per-Application role with no privileges beyond its schema

In (b) this is all ours to design. Nothing in OpenWorkers' database path should be inherited as a security boundary.

## 6. Egress and limits

**Outbound `fetch`** (read in source):

| Rule | Where |
|---|---|
| Guest-controlled URLs use `EgressPolicy::PublicOnly`: a literal non-public IP is refused, and a DNS resolver rejects any hostname that resolves to one (IPv4-mapped IPv6 is unwrapped) | `runner/src/ops.rs:388-441`; `runner/src/services/net_guard.rs:32-41,46-115,125-150,181-205` |
| `FETCH_ALLOW_PRIVATE_NETWORK=1` turns the guard off | `net_guard.rs:22-27` |
| Runner-built targets are `Unrestricted`: S3/R2 binding fetches, worker-to-worker, and internal routing of `*.<WORKER_DOMAINS>` | `ops.rs:356-362,413-427` |
| Redirects are not followed; connect timeout 5 s; read timeout `FETCH_TIMEOUT_MS` | `runner/src/services/fetch.rs:106-120` |
| 50 fetches per request, 6 concurrent (`BindingLimit`), enforced by the runner's limiter | `core/src/limits.rs:73`; `runner/src/limiter.rs:183` |
| No per-worker allowlist or domain policy exists | read in source |

**Outbound websockets.** The literal-IP guard plus the filtering resolver on a dedicated client, with no read timeout (`runner/src/ops.rs:963-1060`; `runner/src/services/fetch.rs:129-176`). They are **not** counted by the fetch limiter (no `limiters()` call in `handle_websocket_connect`).

**embed** has no egress filter by design ("the code is trusted"; `embed/src/fetch.rs:1-3`).

**CPU, memory and wall clock** (read in source):

- **CPU.** Measured per turn with `CLOCK_THREAD_CPUTIME_ID` and charged to the request (`rt/src/security/cpu_enforcer.rs:1-23`).
  - On Linux, a POSIX CPU timer sends SIGALRM and a signal thread terminates the turn (`:128`, Linux module).
  - On macOS, a 5 ms watchdog thread does the job (`:14-18`, `:291`).
  - On other platforms, `get_thread_cpu_time` returns `None`, so **there is no CPU limit**, only the wall clock (`rt/src/security/cpu_timer.rs:43-47`; `cpu_enforcer.rs:47-54`).
  - Script top-level evaluation gets at least 400 ms of CPU (`rt/src/execution_context.rs:36-38,229-233`).
- **Wall clock.** One process-wide watchdog thread holds a map of deadlines and terminates only the request that holds the isolate's turn (`rt/src/security/timeout_guard.rs:1-17`). `waitUntil` work shares the event's budget (`rt/src/execution_context.rs:847-911`).
- **Memory.** A V8 heap limit with a near-heap-limit callback that terminates instead of crashing (`rt/src/security/heap_limit.rs:1-20`), plus a custom ArrayBuffer allocator with its own cap (`rt/src/security/array_buffer_allocator.rs:1-12`). Both are **per isolate**, so they are shared by the requests of one owner (`rt/src/pool_common.rs:40-45`).
- **Runner defaults.**
  - CPU 100 ms (`CPU_TIME_LIMIT_MS`), wall clock 64 s (`WALL_CLOCK_TIMEOUT_MS`), heap 10/50 MB (`runner/bin/main.rs:1143-1146,1311-1324`).
  - In pinned mode these are the pool's limits for every worker. The per-request `config.limits` is used only in `ONESHOT` mode (`runner/src/task_executor.rs:224,300-308`).

## 7. What guest code cannot do

(read in source unless marked)

- **One classic script, no ES modules.**
  - runtime-v8 compiles the code with `v8::Script::compile`, never as a module (`rt/src/execution_context.rs:384-433`). There is no `Module` API or dynamic-import host callback anywhere in `rt/src`.
  - `openworkers-transform` only strips TypeScript and rewrites `export default` and named exports to `globalThis.default` and `globalThis.__exports` (`transform/src/lib.rs:22-130,250-351`).
  - It leaves `import` statements and `export ... from` in place (its own tests, `transform/src/lib.rs:456-475`), and those are syntax errors in a classic script.
  - So the app must be a single pre-bundled file with no top-level `import`.
  - Top-level `await` is unavailable (classic script).
  - Dynamic `import()` has no loader. With no host callback registered, V8 rejects it (V8 behaviour not verified).
- **No Node compatibility.** There is no `process`, `Buffer`, `require` or `node:*` resolution: no match in `rt/src` or `wintertc/js`.
- **No TCP sockets** (`connect()` / `cloudflare:sockets`). Outbound I/O is `fetch`, `WebSocket`, and bindings only.
- **Available globals** come from the wintertc surface plus per-context natives (`wintertc/src/lib.rs:25-193`; `rt/src/runtime/`):
  - Fetch: `fetch`, `Request`, `Response`, `Headers`, `FormData`, `Blob`, `File`
  - Text and URL: `URL`, `URLSearchParams`, `URLPattern`, `TextEncoder`/`Decoder` (+ streams), `atob`/`btoa`
  - Streams: `ReadableStream` (byte streams, BYOB), `WritableStream`, `TransformStream`, `CompressionStream`/`DecompressionStream`
  - Events and messaging: `Event`/`EventTarget`/`CustomEvent`, `AbortController`/`AbortSignal`, `MessageChannel`
  - Other: `structuredClone`, timers, `queueMicrotask`, `performance`, `navigator`, `AsyncContext` / `AsyncLocalStorage`, `WebSocket`/`WebSocketPair`, `console`
  - `crypto.subtle`: digest, HMAC, AES-GCM, RSA, ECDSA, PBKDF2, random (`rt/src/runtime/crypto/`)
  - `SharedArrayBuffer` and `Atomics` are deleted (`rt/src/runtime/bindings/web_api.rs:303-315`).
  - `eval` / `new Function` and WebAssembly are not disabled; no code-generation restriction was found.
- **Consequences for a bundled TypeScript app with Drizzle** (inferred from the above, not verified by running):
  - It must be bundled to a single ES-module file with all imports inlined (esbuild/rollup), then passed through the transform. The `export { x as default }` form that bundlers emit is handled (`transform/src/lib.rs:94-130`).
  - Drivers that need TCP (`pg`, `postgres-js`) or Node built-ins won't run.
  - Drizzle's HTTP/proxy-style drivers could be pointed at our `env.<RESOURCE>.query` or `invoke` op, but no row-shape adapter exists. The runner's database op returns rows as JSON objects (`runner/src/services/database.rs:202-225`).
  - ORM transactions (`db.transaction(...)`) cannot work over a one-statement-per-op API (Q5).
  - Any npm package that touches `Buffer`, `process.env` or `node:crypto` needs polyfills in the bundle.

## 8. Platforms

**OS and architecture support**:

| Item | Evidence | Source |
|---|---|---|
| CI | `ubuntu-24.04` only, for runtime-v8 (fmt, clippy, snapshot, test, feature matrix) and runner (lint, Docker build). There is no macOS or Windows job, and core has no workflow. | `rt/.github/workflows/ci.yml:18-42`; `runner/.github/workflows/{lint,docker}.yml`; `gh api repos/openworkers/openworkers-core/contents/.github/workflows` → 404 |
| Release artifacts | None for runner, runtime-v8 or core: no GitHub releases, tags only. The runner ships as Docker images for `linux/amd64,linux/arm64`. | `gh api repos/openworkers/<repo>/releases` → empty; `runner/.github/workflows/docker.yml:57` |
| V8 prebuilts (fork release v152.2.0) | Linux gnu x86_64/aarch64/riscv64, musl x86_64/aarch64, macOS x86_64/aarch64, and **Windows x86_64/aarch64 MSVC (release, non-ptrcomp only)**. The `ptrcomp` and `sandbox` variants are Linux and macOS only. | `gh api repos/openworkers/rusty-v8/releases` |

**Windows**:
- **Runner.** It does not compile on Windows. It uses `std::os::unix::fs::FileTypeExt` and Unix listeners unconditionally (`runner/src/ingress.rs:8,380-410`) and `libc::statvfs` (`runner/src/d1/mod.rs:972-975`). It also enables `ptrcomp` on runtime-v8 (`runner/Cargo.toml:75`), and there is no Windows ptrcomp prebuilt.
- **runtime-v8.** POSIX code is `cfg`-gated: `libc` is on `cfg(unix)` and `signal-hook` on Linux (`rt/Cargo.toml:67-76`; `rt/src/security/cpu_timer.rs:21,43`; `cpu_enforcer.rs:38-98`). So a Windows build with default features looks possible, but it is **not verified** (never built, no CI). On Windows it would run **without a CPU limit** (Q6).
- **Snapshot path.** The compile-time default snapshot path is `/tmp/openworkers-runtime-v8-snapshot.bin` (`rt/.cargo/config.toml`); a Windows build would need its own.

**Without a container**:
- The runner is a plain binary. The Dockerfile only builds it, generates the snapshot, and copies both into `debian:bookworm-slim` (`runner/Dockerfile`).
- At runtime it needs Postgres (`DATABASE_URL` or `POSTGRES_*` are mandatory, `runner/bin/main.rs:1148-1161`), the OpenWorkers schema, and a D1 data directory (`runner/bin/main.rs:1249-1250`).
- A host built on runtime-v8 needs no container either: embed is a library inside an ordinary Rust program.

## 9. V8 currency

| Fact | Source |
|---|---|
| runtime-v8 depends on `openworkers-v8` 152.2.0 (renamed to `v8`), `openworkers-serde-v8` 152.2.2 and `openworkers-glue-v8` 152 from crates.io | `rt/Cargo.toml:34,49,52` |
| The fork's README says V8 15.2.124.1. Its `v8` submodule at tag v152.2.0 points to `denoland/v8` at `c4ca1ecc`, whose `v8-version.h` says 15.2.124 patch 1 | from metadata: `gh api repos/openworkers/rusty-v8/contents/v8?ref=v152.2.0`, then `include/v8-version.h` |
| Upstream `denoland/rusty_v8` latest release is **v152.2.0 (2026-08-20)**. The fork released v152.2.0 a day later (2026-08-21), so as of 2026-11-11 it is level with upstream's latest release | from metadata: `gh api repos/denoland/rusty_v8/releases`, `.../openworkers/rusty-v8/releases` |
| The fork (not a GitHub fork) is upstream's history up to `v152.2.0 (#2069)` plus about 12 commits by `max-lt` (2026-08-19 to 09-18): build fixes (partition_alloc on arm64+ptrcomp, macOS frameworks, vendored sandbox sources), CI for "the prebuilts upstream does not publish", artifact base pointing at the fork, publishing as `openworkers-v8` | from metadata: `gh api repos/openworkers/rusty-v8/commits` |
| How it is updated: rebase those commits onto the next upstream tag, rebuild prebuilts in the fork's CI, publish `openworkers-v8`, then bump the matching serde-v8/glue-v8 forks and runtime-v8 | `rt/CLAUDE.md` "Development setup"; this procedure is inferred |
| `openworkers/v8` ("floating patches for rusty_v8") is a stale GitHub fork of `denoland/v8`, last pushed 2026-03-07. Its README still names `13.4-lkgr-denoland` and it carries two build patches. It is **not** what the fork's submodule uses | from metadata: `gh api repos/openworkers/v8`, `.../contents/patches` |

## 10. Stability and licences

**core public API across tags** (from git history, `git diff <a> <b> -- src`; counts are lines that add or remove a `pub` item):

| Range | Dates | Changes | Removed / added `pub` lines |
|---|---|---|---|
| v0.14.0→v0.15.0 | 03-29 → 08-21 | ops split into a module (moves), images types | −6 / +27 |
| v0.15.0→v0.16.0 | → 10-12 | D1 types | −0 / +30 |
| v0.16.0→v0.17.0 | → 10-12 | fetch-to-websocket upgrade | −1 / +4 |
| v0.17.0→v0.18.0 | → 10-13 | Durable Objects | −0 / +18 |
| v0.18.0→v0.18.1 | → 10-30 | `invoke` | −0 / +4 |

Core's changes are mostly additive. But each minor adds `Operation`/`BindingType`/`OperationResult` variants to non-`#[non_exhaustive]` enums, so each minor is breaking for exhaustive matches and must be adopted in lockstep with runtime-v8.

**runtime-v8 public API across tags** (from git history):

| Range | Dates | Commits | Removed / added `pub` lines | Lines changed in `src` |
|---|---|---|---|---|
| v0.15.0→v0.16.0 | 08-21 → 10-12 | 86 | −47 / +72 | +3,840 / −6,545 (rewrite: `Runtime`, `WorkerFuture`, `event_loop`, the `multiplexing` feature and the worker snapshot were removed) |
| v0.16.0→v0.17.0 | | 8 | −1 / +2 | |
| v0.17.0→v0.18.0 | | 16 | −1 / +15 (Durable Objects) | |
| v0.18.0→v0.18.7 | → 11-01 | | −0 / +8 | |

There are 112 tags in total, including v0.15.0 to v0.15.23 between 08-21 and 10-11. Monthly commits in 2026 were 15 (Mar), 2 (Jun), 26 (Aug), 37 (Sep), 85 (Oct), 4 (Nov to date).

**Version pins.** Runner v1.18.8 pins core v0.18.1, runtime-v8 **v0.18.6** (not the latest 0.18.7), transform v0.1.4 and postgate v0.1.8 (`runner/Cargo.toml:69-84`).

**Contributors.** The only human contributor on runner (415 commits) and runtime-v8 (343, plus 1 by Copilot) is `max-lt`. All 62 core commits are by `max-lt` (from metadata: `gh api repos/openworkers/<repo>/contributors`; git log).

**Licences** (from metadata: `gh api repos/openworkers/<repo>` `.license`; Cargo files read in source):

| Repo | LICENSE file detected | Cargo `license` |
|---|---|---|
| openworkers-core | MIT | `MIT` (`core/Cargo.toml:5`) |
| openworkers-runtime-v8 | MIT | `MIT` (`rt/Cargo.toml:8`) |
| openworkers-runner | MIT | `MIT` (`runner/Cargo.toml:5`) |
| openworkers-wintertc | MIT | `MIT` |
| rusty-v8 (openworkers-v8) | MIT | `MIT` |
| **openworkers-embed** | **none** | `MIT` (`embed/Cargo.toml:5`) |
| **postgate** | **none** | `MIT` (`postgate/Cargo.toml:7`) |
| **openworkers-transform** | **none** (also needed for the guest pipeline) | `MIT` (`transform/Cargo.toml:5`) |
| **openworkers-infra** | **none** | no Cargo.toml (compose files and shell scripts) |
| serde-v8, glue-v8 | none detected by GitHub (`license: null`) | `MIT` (`gh api .../contents/Cargo.toml`) |

## Judgement on the three options

**(a) Thin patch on the runner: not favoured.**

What the runner gives us:
- Ingress with header stripping, the SSRF guard, binding limiters, warm pools, and log plumbing.

What decides against it:
- A session hook and identity threading touch five places across `main.rs`, `event_fetch.rs`, `task_executor.rs` and `ops.rs` (Q4).
- We would also inherit, and then have to undo:
  - the one-runner-per-database lock (`runner/src/singleton.rs`)
  - the OpenWorkers platform schema as our control plane
  - guest SQL on the runner's own role, confined by `search_path` only (Q5)
  - tenant-level isolate sharing (`owner_id = user_id`)
  - one process-wide limit set (Q6)
  - D1, Durable Objects and crons, about 9,000 lines we would not use
  - Linux-only builds
- Every upstream release would conflict with our patches, in the largest and fastest-changing repo.

**(b) Own host on core + runtime-v8: favoured.**

The evidence:
- The trait boundary has default stubs (`core/src/ops/mod.rs:304-551`).
- A second host (embed) is built entirely on public runtime APIs (`embed/src/engine/warm.rs`).
- Our SQL op needs no runtime change (`database` or `invoke` binding, Q3).
- Per-request identity is a field in our own handler, set before `exec`. It is safe because a context is never shared by two in-flight requests (Q2).
- We choose the pool shape:
  - `Worker` per request for maximum isolation
  - `execute_pinned` with `on_warm_hit`
  - or our own pool on `ExecutionContext`, as embed does, to get per-Application limits
- The work is ingress, session, handler and audit. That is close to embed's size plus our own logic, not the runner's 20k lines. The runner's `net_guard.rs` and `limiter.rs` are worth copying under MIT.

The biggest risk:
- **Upstream volatility and bus factor.** runtime-v8's public API was rewritten two months ago, and the V8 supply chain runs through one person's forks.
- Mitigation: pin exact tags, vendor the crates, and plan to own V8 bumps (rebase about 12 build commits onto upstream rusty_v8 and rebuild prebuilts).

Secondary risks:
- warm reuse keeps guest globals across users
- no CPU limit on Windows
- no ES modules or Node APIs, which constrains which npm packages and ORMs work (Q7)

**(c) Fork and rewrite freely: a fallback, not the plan.**

MIT allows it, and it is how the risk in (b) would end if upstream stalls. But nothing in the source *requires* changing core or runtime-v8 for identity or our SQL operation, so forking on day one would only take on the V8 and fork maintenance early. Fork when you need one of these:
- per-request limits inside the pinned pool
- an identity-carrying op signature
- removal of the Durable Object and D1 surface

## What could not be verified

- That runtime-v8 compiles or runs on Windows. Nothing was built, and there is no Windows CI.
- V8's behaviour for `import()` with no host callback.
- Postgres semantics of `set_config` / `role` under the runner's wrapping.
- Whether Drizzle's proxy drivers fit the `query` result shape.
- The "tens of µs" context creation cost claimed in `rt/src/execution_context.rs:7`.
- Line counts include inline tests and comments. No performance numbers were measured.
