# Vibe Manager (working title)

An open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure. Think PocketBase for the enterprise, built for the AI coding era.

**Status: building Milestone 1.** The program is scaffolding only so far.

## Build and test

One Cargo workspace at the root; the Rust version is pinned in `rust-toolchain.toml` (rustup installs it). The first build downloads a V8 prebuilt.

Prerequisites: a C compiler and libclang. The statement parser (`pg_query`, Postgres's own parser compiled in) builds C code and runs bindgen, which loads libclang. Either install libclang for the whole system (Debian and Ubuntu: `apt install clang libclang-dev`; macOS: the Xcode Command Line Tools; Windows: LLVM from llvm.org), or, on Linux without root, run

```
scripts/fetch-libclang.sh   # libclang from the PyPI wheel into .tools/libclang (git-ignored)
```

`.cargo/config.toml` adds `.tools/libclang` to where the build looks, so a plain `cargo build` and `scripts/gate.sh` find it without any variable exported by hand. A `LIBRARY_PATH` or `CLANG_PATH` already set in your shell takes precedence over that file.

```
cargo build              # target/debug/vm
target/debug/vm --version
```

The gate is `scripts/gate.sh`: it checks that `blackbox` depends on no product crate, then runs

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- `crates/vm`: the platform program, one binary named `vm` (ADR 0015).
- `blackbox`: the black-box tests, which start the real `vm` and depend on no product crate (see "How code gets written" in `AGENTS.md`).
- `vendor/openworkers-runtime-v8`: the patched execution core (`vendor/VENDORED.md`).

- [Vision](docs/vision.md)
- [Glossary](CONTEXT.md)
- [Architecture decisions](docs/adr/)
- The road to a build-ready Milestone 1 spec is charted on the issue tracker: see the issue labelled [`wayfinder:map`](../../issues?q=label%3Awayfinder%3Amap).
