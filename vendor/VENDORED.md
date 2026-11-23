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

Every upstream upgrade must re-apply every patch below.

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

### Patch 4: a script that throws while loading reports its stack

Upstream reported an error thrown by a script's top level as the error's text alone (`Error: message`), so a host could not say where the script failed.

| File | Change |
|---|---|
| `src/execution_context.rs` (`evaluate_script`, both the source and the code-cache branch) | `TerminationReason::Exception` carries the error's stack (`TryCatch::stack_trace`) when it has one, and its text otherwise, as before. |

### Patch 5: arguments of a host call that throw while read do not abort the process

Upstream read the arguments of `__nativeBindingInvoke` with `serde_v8`, which unwraps every property read. A property read that throws (a getter, a Proxy trap) panicked inside the V8 callback, and a panic there cannot unwind, so the whole process aborted: one Application could stop every other one. Found by the hostile fixture's "an argument whose getter throws".

| File | Change |
|---|---|
| `src/runtime/bindings/fetch.rs` (`setup_invoke`) | The arguments are read with `v8::json::stringify` and parsed with `serde_json`. What the guest's code throws while they are read goes back to the guest as the call's exception; `undefined` and what JSON cannot hold become `null`. |

The runtime's other natives (`__nativeBindingFetch`, the KV, storage and database ones) still read their arguments with `serde_v8`. This program serves no binding of those kinds, and the hostile fixture calls them with plain values only, so they were not changed here; they are worth the same patch before any of them is served.

### Patch 6: the host learns which host-fed stream became the response's body

The platform sends a body the host itself feeds and the code did not read (today the request's own body given back, `new Response(request.body)`) from the connection itself, so that it goes out after the code has ended and never passes through JavaScript or is collected (ADR 0019, "Bodies: a handle and a stream"). A native stream given as a `Response`'s body already arrives at the host as a `ResponseBody::Stream` without passing through JavaScript (`js/stream_body.js`, `read_response_object`); what the host could not tell was whether that stream was one the host fed (and not some `ReadableStream` of the code's own) and whether the code had read from it. Only both together make it the host's to send: a body the code began to read is no longer one the host can vouch for.

| File | Change |
|---|---|
| `src/runtime/stream_manager.rs` | `StreamManager` keeps the host-fed streams and whether the guest has read from each (`mark_host_fed`, set by `pump_request_body`; `read_chunk` notes a read; `host_fed_read`), and the response's host-fed body (`note_host_response`, `host_response`, a `HostResponse { read }`). All of it is cleared by `clear()` with the rest of a context's streams. |
| `src/execution_context.rs` (`trigger_fetch_event`, phase 3) | Before the response is sent to the host, the response's `_responseStreamId` (the existing `get_response_stream_id`) is looked up in the manager; when it is a host-fed stream the manager notes it and whether it was read. |

The host reads `host_response()` through `Worker::stream_manager()` (no new API on `Worker`) after `exec`, before the context is dropped: a host-fed body the code did not read is sent by the host as it is; every other body is collected as before. The response's bytes are unchanged.

`pump_request_body` has two callers, and the marking reaches both: `src/execution_context.rs` (`trigger_fetch_event`, an HTTP fetch — the path this program serves) and `src/execution_context/durable_object.rs` (a Durable Object's fetch, which this host does not expose).

### Patch 7: a chunk the code writes to the host is at most 64 KiB

| File | Change |
|---|---|
| `src/runtime/bindings/streams.rs` (`response_stream_write`) | A chunk larger than `stream_manager::MAX_CHUNK_BYTES` (65,536) is refused with an `Err`, which the `glue_v8` method wrapper throws into the guest. |
| `src/js/stream_body.js` (`streamBody`) | The platform's own response pump cuts a larger chunk into pieces of at most 64 KiB before writing each, so code that answers a body of its own is not refused for its chunk sizes. |

### Patch 8: at most 16 streams of the code's own towards the host for one request

| File | Change |
|---|---|
| `src/runtime/stream_manager.rs` | `MAX_GUEST_STREAMS` (16) and the set of the streams of the guest's own made in the context (`check_guest_stream`, `mark_guest_stream`; cleared by `clear()` with the rest). The set is also what patch 12 refuses every other stream id against. |
| `src/runtime/bindings/streams.rs` (`response_stream_create`) | The 17th `__responseStreamCreate` is an `Err`, which the `glue_v8` method wrapper throws into the guest. Called from the guest's handler, the throw is the handler's and the request is the fixed `500` (measured). Called from the platform's response pump (`js/stream_body.js`), after the guest made 16 streams itself, the throw ends the pump without an answer: the event then fails with `No response set` and the request is the fixed `500` too (measured, about 9 s after the request began). |

The set is per context. This program makes one context per request (the runner has no pool), so the bound is one request's, as the contract has it; under the runtime's own pooled path `clear()` resets it between requests.

### Patch 9: the request's body is read only when it is asked for

CONTRACT.md, "What an Application is handed": "The request's body is not read before the code asks for it or hands it on ... A body nobody has asked for yet is not read and not timed ... A client that sent `Expect: 100-continue` is told `100 Continue` only when the body is asked for". Hyper sends `100 Continue` when the body is first polled, so the host's pump must not poll it before the guest asks. The runtime already knows the two moments: a guest read of a host-fed stream (`read_chunk`) and the response taking a host-fed stream as its body (`take_receiver`).

| File | Change |
|---|---|
| `src/runtime/stream_manager.rs` | `StreamManager` keeps an `asked` sender (`watch_asked`, `mark_asked`); `read_chunk` marks it for a host-fed stream, and `take_receiver` marks it when the stream taken is host-fed. `clear()` leaves it (the host attaches one per request; a reset must not tell the host's pump that nobody asked). |
| `src/execution_context.rs` (`Worker::stream_manager` users) | No change here: the host attaches the sender (`watch_asked`) before the event, and reads the channel in its own pump task. |

The host's pump (`crates/vibe/src/ingress.rs`, `read_body`) waits on the channel before its first `frame()`; the wait ends with the request because the sender is dropped when the request is never given to the code (the runner answers `Busy`, or the Application is disabled while the request waits). Marking affects `pump_request_body`'s two callers alike: `src/execution_context.rs` (an HTTP fetch, this program's path) and `src/execution_context/durable_object.rs` (a Durable Object's fetch, not exposed by this host).

### Patch 10: an answer with no body on the wire does not take a stream

CONTRACT.md, "What an Application is handed": "An answer that has no body on the wire has none whatever the code answered: the answer to a `HEAD`, and one whose status is `204` or `304` ... a body given back is not asked for".

| File | Change |
|---|---|
| `src/execution_helpers.rs` (`read_response_object`) | Takes whether the request was a `HEAD`; for a `HEAD`, `204` or `304` answer it closes the answer's stream (`close_stream`) instead of taking its receiver, and answers `ResponseBody::None`. A host-fed body given back is therefore never marked asked and never read from the client. |
| `src/execution_context.rs` (`trigger_fetch_event`, phase 3) | Passes `req.method == HttpMethod::Head`. |
| `src/execution_context/durable_object.rs` | `FetchInFlight` keeps whether the request was a `HEAD`, passed the same way. |

### Patch 11: what a request's body held is given back when the request ends

CONTRACT.md, "What an Application is handed": "What the program held for a request's body is given back when the request ends". `pump_request_body` spawns a task that forwards the host's channel into the StreamManager's stream. When the guest does not read the stream to its end, and the response does not take it, the stream's receiver stays in the manager's own map, so the task blocks writing into a channel nobody reads — and it holds the manager, so both stay for the life of the process (measured: twenty requests whose body was never read and whose code ran two seconds each left twenty tasks and about 18 MiB resident). The body's bytes are not lost or duplicated by ending it: the request is over.

| File | Change |
|---|---|
| `src/runtime/stream_manager.rs` | `close_unread_host_fed()` closes every host-fed stream **the response did not take** (`taken`, set by `take_receiver`), whatever the receivers map says: `read_chunk` takes the receiver out for the whole `recv()` and puts it back afterwards, so a read the guest started and did not wait for is out of the map when the request ends. The put-back checks for a sender and inserts the receiver **under the one `senders` lock**, and `close_stream` removes the sender and then the receiver **under that same lock**, so no interleaving of the two leaves a receiver in the map for a stream with no sender. `close_stream` and `close_unread_host_fed` take every lock with `unwrap_or_else(PoisonError::into_inner)`: both are reached from `RequestContext::drop` (Rust rule 4: no panic there). |
| `src/request_context.rs` | `Drop for RequestContext` calls it: the context is dropped when the request's worker is dropped (after the event, or when a disable ends the request), so the task ends then — or, for a body being sent, when the host's receiver goes (the forwarder closes the stream). Measured after the patch: every body ending tried (never read, read in part, read whole, given back and sent, given back after a cancel, cancelled then a made body, disabled, client gone, a read started and not awaited) ends the task; a count of live pump tasks and live managers read 0 after 200 requests of each shape. |

### Patch 12: only a stream the guest made itself may be written, ended, failed or waited on

CONTRACT.md, "Storage: Bounds", "Streams of its own": "`__responseStreamWrite(id, chunk)` with an `id` that is not a stream the code itself made with `__responseStreamCreate()` in this request (the request's body, a file's body, a number it made up): nothing is written. So has `__responseStreamWrite(id, chunk)`, and every other call of the runtime's own that acts on a stream towards the host (`__responseStreamEnd`, `__responseStreamError`, `__nativeScheduleWritable`), with an `id` that is not a stream the code itself made …: nothing is written, ended or closed." Before this patch `__responseStreamWrite` took any id: with `request.body._nativeStreamId` a guest could write bytes into the request's own body stream and the host would send a body length that disagreed with the `content-length` it stated — the lying length the framing rule exists to prevent. `__nativeScheduleWritable` took any id too, and the scheduler's wait closes the stream it was given when the request is cancelled: with the request body's id the guest could have the host's body cut.

| File | Change |
|---|---|
| `src/runtime/stream_manager.rs` | `is_guest_stream`, `is_host_fed`; `guest_streams` is the set patch 8 keeps. `cancel_stream`: a host-fed stream **the response has not taken** is closed for good, marked used (`host_fed[id] = true`, so patch 6 says it was read and the host never sends it back), and **not asked for** (nothing is read from the client for it, no `100 Continue`); a host-fed stream the response **took** belongs to the host and the cancel is ignored; any other stream closes as before. Ignoring every cancel was wrong: the code could cancel the request's body and then give it back and the host would stream the client's bytes to it. `read_chunk` takes the receiver **before** it notes the stream read and asks for it, so a read that finds nothing (after a cancel, or a second reader) neither asks the client for the body nor marks it read. |
| `src/runtime/bindings/streams.rs` (`response_stream_write`, `response_stream_end`, `response_stream_error`) | Each takes the id and refuses it with an `Err` (thrown into the guest) unless `StreamManager::is_guest_stream(id)` — an id `__responseStreamCreate` made in this context. Nothing is written, ended or failed on refusal. `__responseStreamIsClosed` is unchanged: it only reads whether the host still holds a sender for the id (a host-fed stream reads as open while the host has it), and closing it is not something the guest can do through it. |
| `src/runtime/bindings/streams.rs` (`native_stream_cancel`, `setup_stream_ops`) | `__nativeStreamCancel` calls `StreamManager::cancel_stream` **synchronously, inside the guest's call**, and still sends the scheduler's `StreamCancel` message: the guest may cancel its body and give it back in the same handler, and the response takes its stream before the scheduler would get to a message. `setup_stream_ops` takes the manager and keeps it in `StreamState`. |
| `src/runtime/bindings/state.rs` (`StreamState`) | `StreamState.manager`: the streams, so a cancel can act on them inside the call. `TimerState.manager` likewise, for the wait. |
| `src/runtime/scheduler.rs` (`SchedulerMessage::StreamCancel`) | The arm calls `StreamManager::cancel_stream` (the native has already done so; the message is a second, idempotent path). |
| `src/runtime/bindings/timers.rs` (`native_schedule_writable`, `setup_timers`) | `__nativeScheduleWritable(id, stream_id)` refuses a `stream_id` that is not a stream the guest made, with an `Err` thrown into the guest; nothing is scheduled and nothing is closed. It is no longer a `fast` call (a fast call cannot throw): the arguments are the timer id and the stream id, as `js/stream_body.js` calls them. `setup_timers` takes the manager for the check. |
| `src/execution_context.rs` (the two setup calls) | Passes `stream_manager.clone()` into `setup_stream_ops` and `setup_timers`. |

Measured with the fixtures: calling `__responseStreamWrite`, `__responseStreamEnd` or `__responseStreamError` on `request.body._nativeStreamId` and then giving the body back throws (the message names the id and `__responseStreamCreate`) and the answer is exactly the client's bytes with the stated length; a cancel followed by a made answer gives the made answer, with no `100 Continue` and no byte taken from the client, and a cancel followed by giving the body back gives `200` with an empty body and `content-length: 0`; `__nativeScheduleWritable(timer, request.body._nativeStreamId)` throws and nothing is closed; `__responseStreamIsClosed` answers `false` for a host-fed stream the host still holds.

### Patch 13: a body whose source ends without its end is failed, not ended

The forwarder between the StreamManager and the host (`read_response_object`) turned the end of a stream's *sender* (`recv()` answering `None`, with no `Done` on the channel) into a clean end of the response body. For a body the host sends that is a lie: with a stated `content-length` hyper sees a short body and closes the connection, but with a chunked answer it would write the terminating chunk and claim whole a body that is short — the same "clean end short of what was sent" a cancelled body must never produce. The request's body given back after a cancel is answered by the runtime closing the stream (`patch 12`), so this arm is about a taken body whose source goes away.

| File | Change |
|---|---|
| `src/execution_helpers.rs` (`read_response_object`, the forwarder's `chunks.recv()` arm) | `None` without a `Done` now sends `Err("the body's source ended without its end")` on the channel instead of breaking silently: the sending ends early and the connection is closed without a clean end. `Some(Done)` is still a clean end and `Some(Error)` still forwards its error. |

Measured with the cancel tests: a cancelled body given back is `200` with an empty body and `content-length: 0` (or the fixed `500`), never the client's bytes, and never a clean end short of a stated length.
