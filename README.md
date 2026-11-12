# Vibe Manager (working title)

An open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure. Think PocketBase for the enterprise, built for the AI coding era.

**Status: building Milestone 1.** The program is scaffolding only so far.

## Build and test

One Cargo workspace at the root; the Rust version is pinned in `rust-toolchain.toml` (rustup installs it). The first build downloads a V8 prebuilt.

```
cargo build              # target/debug/vm
target/debug/vm --version
```

The gate, as continuous integration runs it (`.github/workflows/ci.yml`):

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
