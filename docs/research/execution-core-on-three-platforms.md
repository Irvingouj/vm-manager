# Execution core on Linux, macOS and Windows: findings

Date: 2026-11-11/12. Code: branch `spike/execution-core-cross-platform`, under `prototypes/execution-core/`. Resolves the map ticket "Run the execution core on Linux, macOS and Windows".

Provenance: this is the report of the engineering agent that did the work, saved as returned. The driving session independently checked the three CI runs and their Windows logs, the size and location of the patch, the kept LICENSE file, and that the branch touches nothing outside `prototypes/execution-core/` and the workflow file. Everything else is the agent's account.

**Result: the OpenWorkers execution core (`openworkers-core` v0.18.1 + `openworkers-runtime-v8` v0.18.7) builds and runs natively on Linux x86_64, macOS arm64 and Windows x86_64 (MSVC). Our minimal host serves a guest's `fetch` handler, and the guest calls a host function, on all three. Upstream needed no change to build or run on any of them. The one real gap was Windows: no CPU-time limit. We patched it in with a small change, and a busy loop is now cut at about 50–70 ms on all three platforms.**

How each claim is known:

- **[here]**: ran on the development machine (Ubuntu 22.04.5, Linux 6.8 x86_64, Docker 29.2.1, rustc/cargo 1.99.1).
- **[CI baseline]**: [run 43142977421](https://github.com/Irvingouj/vm-manager/actions/runs/43142977421), commit `de0feba`, unmodified upstream.
- **[CI patched]**: [run 43143637239](https://github.com/Irvingouj/vm-manager/actions/runs/43143637239), commit `7d6652d`.
- **[CI final]**: [run 43144318280](https://github.com/Irvingouj/vm-manager/actions/runs/43144318280), commit `fa2ad23`.
- **[source]**: read in source, not run.
- **[not verified]**: not checked.

## 1. State per platform

| | Linux x86_64 | macOS arm64 | Windows x86_64 MSVC |
|---|---|---|---|
| Builds, unmodified upstream | yes [here, CI baseline] | yes [CI baseline] | **yes** [CI baseline] |
| Guest serves; host function called via `invoke` | yes | yes | yes [CI baseline] |
| Busy loop cut by the CPU limit, unmodified | yes, 50.8 ms | yes, 68.7 ms | **no: `WallClockTimeout` after 10.0 s** [CI baseline] |
| Busy loop cut by the CPU limit, patched | yes, 50.6–51.0 ms | yes, 54–66 ms | **yes, 52.7–68.7 ms** [CI patched, CI final] |
| Worker serves again after the cut | yes | yes | yes |
| Snapshot via `EC_SNAPSHOT` (`set_snapshot`) | yes | yes | yes [CI patched] |
| Runner image | ubuntu-26.04 | macos-26-arm64 | windows-2025-vs2026 |
| `cargo build --release`, cold | 58 s [CI baseline]; 31 s [here] | 107 s [CI baseline] | 160 s [CI baseline] |
| `cargo build --release`, warm cache | 11 s | 21 s | 53 s [CI final] |
| `ec-host` binary (release, `strip="debuginfo"`) | 83,266,760 B | 75,892,464 B | 60,499,456 B [CI patched] |
| Snapshot size | 619,424 B | 688,248 B | 619,584 B |
| Worker ready (isolate + context + guest top level) | 3.4–4.9 ms [here] | not measured | 9.5 ms [CI baseline] |

Request and response on the development machine [here]:

```
$ curl -i 'http://127.0.0.1:8787/?name=Ada'
HTTP/1.1 200 OK
content-type: application/json
{"guest":"hello from the guest","host":{"arch":"x86_64","message":"hello, Ada, from the host","os":"linux"}}
$ curl -i http://127.0.0.1:8787/spin
HTTP/1.1 503 Service Unavailable
guest stopped: CpuTimeLimit        (50.7 ms)
```

**V8 prebuilts.**

- The fork's release v152.2.0 publishes `rusty_v8_release_{x86_64,aarch64}-pc-windows-msvc.lib.gz` [here, `gh release view v152.2.0 -R openworkers/rusty-v8`].
- The build script downloaded it on Windows with no configuration [CI baseline].
- There is no Windows `ptrcomp` or `sandbox` prebuilt; the host uses default features.

**LNK4098.** Every Windows link prints `LNK4098: defaultlib 'libcmt.lib' conflicts with use of other libs`. It is a warning only and the binary runs. Cause not investigated [not verified].

## 2. What was built (`prototypes/execution-core/`)

- **`host/src/main.rs`**: the minimal host.
  - A hyper 1 HTTP/1 server.
  - An engine thread (current-thread tokio runtime + `LocalSet`) runs one warm `Worker` serially.
  - `HostOps::handle_binding_invoke` answers `HOST.greet` and checks the binding name.
  - Knobs: `EC_CPU_MS` (50), `EC_WALL_MS` (10000), `EC_SNAPSHOT`.
  - `ec-host snapshot [path]` writes the V8 snapshot.
  - It follows `openworkers-embed`'s approach but uses no embed code (embed has no LICENSE file).
- **`guest/hello.js`**: a classic script with `globalThis.default = { fetch }`.
  - `/` calls `env.HOST.invoke("greet", {name})`.
  - `/spin` is `while(true){}`.
  - `openworkers-transform` is not used.
- **`smoke.sh`**: bash on all three (Git Bash on Windows). It checks:
  - the host function's answer and the OS it reports;
  - that `/spin` gets 503 `CpuTimeLimit` while the wall clock is 10 s;
  - that the worker serves again afterwards.
- **`host/tests/cpu_limit.rs`**: the busy loop on `Worker` must end in `CpuTimeLimit` within 2 s, and the next request must answer.
- **`lockdown/`**: Dockerfile and `lockdown.sh`.
- **`.github/workflows/execution-core.yml`**:
  - matrix of ubuntu, macos and windows;
  - triggers on pushes to the spike branch touching `prototypes/execution-core/**` or the workflow;
  - Rust 1.99.1, `Swatinem/rust-cache` with `cache-on-failure` (the V8 prebuilt lives in `target/`);
  - steps: build, write the snapshot, smoke, smoke with `EC_SNAPSHOT`, `cargo test`.

## 3. Patches

All are in `vendor/openworkers-runtime-v8/`, from upstream commit `4ea28c1c26ea10920ad71b8e7bec264f735f7e6e` (v0.18.7, MIT), wired in with `[patch]`. See `vendor/VENDORED.md` on the branch. To see the diff: `git diff 8193489 7d6652d -- prototypes/execution-core/vendor`. `openworkers-core` is unmodified (tag v0.18.1).

1. **CPU-time limit on Windows** (`cpu_enforcer.rs`, `cpu_timer.rs`, `windows-sys` 0.61).
   - **Why.** Upstream has a POSIX timer + SIGALRM on Linux and a 5 ms watchdog thread on macOS. Elsewhere `get_thread_cpu_time()` returns `None`, so there is no CPU limit [source]. Confirmed: `WallClockTimeout` after 10 s [CI baseline].
   - **Change.** The macOS watchdog becomes `watchdog`, built for macOS and Windows. Only the per-thread CPU read is per OS, in `sys`:
     - macOS: the Mach code moved verbatim.
     - Windows: an `OpenThread(THREAD_QUERY_LIMITED_INFORMATION)` handle per thread, closed when its slot drops, read with `GetThreadTimes` (kernel + user).
     - `get_thread_cpu_time()` on Windows reads `GetThreadTimes(GetCurrentThread())`.
     - Fail-closed behaviour is shared unchanged.
   - **Result.** Windows cuts at 52.7–68.7 ms with a 50 ms budget. macOS still cuts at 54–66 ms. Linux untouched [CI patched, CI final].
   - **Caveat.** `GetThreadTimes` reportedly advances in ticks of about 15.6 ms, so expect overshoot of up to roughly one tick plus the 5 ms watchdog tick [not verified; observed overshoot 3–19 ms]. `QueryThreadCycleTime` would be precise but needs a cycles-to-time calibration.
   - **Not run on Windows.** The upstream in-module watchdog unit tests [not verified].
2. **`build.rs`.**
   - Upstream `File::create(RUNTIME_SNAPSHOT_PATH).unwrap()` panics when the directory is missing. Reproduced with `/app/runtime-snapshot.bin` [here].
   - It would also panic on Windows with upstream's default `/tmp/...` if `\tmp` is absent [not verified].
   - Now a `cargo:warning`.
3. **`platform::set_snapshot(&'static [u8]) -> bool`.**
   - The host supplies the snapshot at run time; empty bytes mean none.
   - `ec-host` uses it with `EC_SNAPSHOT`, checked on all three [CI patched].

Not patched:

- **The host must set `RUNTIME_SNAPSHOT_PATH`, or the build fails.** runtime-v8's own `.cargo/config.toml` is not read when it is a dependency. The error is `environment variable RUNTIME_SNAPSHOT_PATH not defined at compile time` (`build.rs:4`) [here]. The prototype's `.cargo/config.toml` sets it relative to the workspace, so the absolute path is baked per machine (`D:\a\...\runtime-snapshot.bin` on Windows).
- **A missing snapshot is harmless.** It cost about 1.5 ms of worker start: 4.9 ms against 3.4 ms, one sample each [here].

## 4. Lockdown (Linux, Docker) [here]

`lockdown/lockdown.sh` builds with `RUNTIME_SNAPSHOT_PATH=/app/runtime-snapshot.bin` on `gcr.io/distroless/cc-debian12:nonroot`, then runs:

```
docker run -d --read-only --cap-drop ALL --security-opt no-new-privileges --ipc none \
  --user 65532:65532 --pids-limit 64 --memory 256m -p 127.0.0.1:18787:8787 ec-host-lockdown
```

`docker inspect`:

```
ReadonlyRootfs=true CapDrop=[ALL] CapAdd=[] SecurityOpt=[no-new-privileges] Mounts=0 Ipc=none Tmpfs=map[] User=65532:65532 Ports=map[8787/tcp:[{127.0.0.1 18787}]]
```

Results:

- `/?name=lockdown` → 200 with the host function's answer.
- `/spin` → 503 `CpuTimeLimit` in 50.2 ms. The POSIX CPU timer and SIGALRM work under the default seccomp profile with no capabilities.
- The snapshot loaded from `/app`.
- A write in the container (`ec-host snapshot /app/x.bin`) fails with `Read-only file system (os error 30)`.

**Nothing had to be relaxed.** The snapshot snag did not bite:

- reading a file from a read-only filesystem works;
- building with the container path needs patch 2.

Not tried [not verified]: a seccomp profile tighter than the default, a static musl build, egress control.

## 5. How to repeat

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
rustup default 1.99.1
cd prototypes/execution-core
export CARGO_TARGET_DIR=/tmp/ec-target
cargo build --release --locked
$CARGO_TARGET_DIR/release/ec-host snapshot
./smoke.sh $CARGO_TARGET_DIR/release/ec-host
cargo test --release --locked
lockdown/lockdown.sh
```

Pinned versions (`Cargo.lock`):

- `openworkers-core` v0.18.1 `0bdc4e4e`
- `openworkers-runtime-v8` v0.18.7 `4ea28c1c` (vendored)
- `openworkers-wintertc` v0.1.11 `51df92ac`
- `openworkers-v8` 152.2.0, `openworkers-serde-v8` 152.2.2, `openworkers-glue-v8` 152.2.0

## 6. Against the earlier source reading (`openworkers-host-layer.md`)

- **Windows build was unknown.** It builds and runs unmodified [CI baseline].
- **No CPU limit on Windows.** Confirmed, and patched.
- **Windows prebuilt.** It exists and is used automatically. Not a wall.
- **Compile-time snapshot path.** Confirmed, with two consequences the reading missed:
  - the host must define `RUNTIME_SNAPSHOT_PATH`;
  - `build.rs` panics when the directory is absent.
- **Classic script.** Consistent.
- **embed as the model.** Followed in approach only; not used, because it has no LICENSE file.

## 7. Still unknown

- Windows arm64 and macOS x86_64.
- The precision of the Windows CPU limit under load, and whether `QueryThreadCycleTime` is worth the calibration.
- The upstream watchdog unit tests on Windows.
- Real employee Windows machines: Defender or EDR, non-admin users, paths with spaces.
- Concurrency and pooling (`execute_pinned`, several threads).
- The guest's outbound `fetch`, the `database` binding, and `openworkers-transform`.
- Whether a snapshot is portable between machines.
- The cause of LNK4098.
