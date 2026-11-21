# Making a release

A release is the image on GitHub's container registry and a GitHub release with the Compose files and the tool for Linux and macOS (`blackbox/CONTRACT.md`, "The first release"). It is made by the workflow `.github/workflows/release.yml` from a commit of `main`. Nothing is published by hand.

## Before the tag

1. Choose the version, `<version>` below (for example `0.2.0`).
2. Change it in every place that names it:
   - `crates/vibe/Cargo.toml` (the package `vibe`), then `cargo build` so that `Cargo.lock` follows;
   - the `Dockerfile`: `ARG VIBE_VERSION=<version>`;
   - `deploy/compose.yaml` (`${VIBE_IMAGE:-ghcr.io/irvingouj/vibe:<version>}`) and `deploy/.env.example`;
   - `docs/getting-started.md`: every address `.../releases/download/v<version>/...` and every archive `vibe-<version>-<target>.tar.gz`.
3. Write the release's notes in `docs/releases/v<version>.md`: what works, what is missing (the open issues, in a user's words), and what was and was not tried.
4. Run `scripts/release-check.sh`. It names each place that does not agree, and says when `LICENSE` is missing. Run it until it passes.
5. Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and the test files that touch what the release changes (`AGENTS.md`, "The loop for every ticket", step 6). The whole gate (`scripts/gate.sh`) and the install gate (`scripts/install-gate.sh`) are run only when the product owner asks. Say in the notes what was run.
6. Commit, and push to `main`.

## A rehearsal

Start the workflow by hand on `main` (on GitHub: Actions, "release", "Run workflow"; or `gh workflow run release.yml --ref main`). It runs every job but `publish`: the release check (without a tag, so a missing `LICENSE` is said and does not fail it), the image built, the tool built, packed and tried on Linux and on macOS, and the release's files. Its files are kept for 7 days as the run's artefacts: `image` (the image that was built, as `docker save` writes it), `tool-<target>` (each archive), and `release-files` (everything the release would hold, with `SHA256SUMS`).

## The release

Push the tag on the commit of `main` you rehearsed:

```sh
git tag -a v<version> -m v<version> <commit>
git push origin v<version>
```

The workflow then runs for the tag. `scripts/release-check.sh v<version>` also checks that the tag is `v<version>`, that its commit is on `main`, and that `LICENSE` exists: nothing is published without the program's own licence. When every job has passed, `publish`:

1. stops if a release `v<version>` is already published: a published release is never overwritten;
2. pushes the image the install tests passed (the same image, by its id) as `ghcr.io/irvingouj/vibe:<version>`;
3. checks that the image can be pulled without a sign-in;
4. makes the release `v<version>` as a draft with every file, its text `docs/releases/v<version>.md`, then publishes it.

## When a run fails

When a job was lost to something outside the repository (a download that failed, a runner that went away), wait for the run to end and run only what failed again: `gh run rerun <run id> --failed`. The jobs that passed keep their results, and `publish` runs once every job before it has passed. The run for `v0.1.0` was published so: its image job had been answered `404` for a download of the test harness, and passed when run again.

Otherwise fix the cause, and run the workflow again for the same tag, by hand ("Run workflow", and choose the tag `v<version>` instead of a branch). A draft left by the failed run is replaced. If the fix needs a new commit, the tag must move to it: delete the tag (`git push origin :refs/tags/v<version>` and `git tag -d v<version>`), and tag the new commit of `main`.

After pushing the image, the run checks that anyone can pull it without a sign-in, and makes the release only then. GitHub's documentation says a package is private when first published; for `v0.1.0` that did not happen: the image, pushed by this public repository's workflow, could be pulled without a sign-in seconds after the push, and nobody had touched the package. Should a run ever stop there, saying that the image cannot be pulled without a sign-in: make the package public once (on GitHub: your profile, Packages, `vibe`, Package settings, Change visibility, Public), and run what failed again (`gh run rerun <run id> --failed`).

## What the workflow runs on

- `check`, `image`, `files` and `publish` on `ubuntu-24.04`. The image job first removes large tools the repository does not use (Android, .NET, GHC, CodeQL, Swift), so that the image's build fits on the disk, and stops if less than 15 GB is free.
- **No gate and no install tests** (the product owner's decision of 2026-11-21, until the product is in use): the workflow checks the versions, builds the image (`docker build`) and the tool, tries the packed tool (`scripts/try-tool.sh`) and publishes. `scripts/gate-hosted.sh` (the whole gate once with `--no-fail-fast`, then each test file that failed once more, alone and one test at a time, because the gate's time limits are sized for 12 cores and the runner has 4) and `scripts/install-gate.sh` stay in the repository with their tests. When the release gate comes back, the gate job must first allow unprivileged user namespaces (`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`: Ubuntu 24.04 restricts them through AppArmor and `blackbox/tests/local_offline.rs` needs one), needs `libclang-dev`, and prints the runner's Firefox version; the commit before this decision has those jobs as they last ran (`git log -- .github/workflows/release.yml`).
- The Linux tool on `ubuntu-22.04`, whose glibc is 2.35: the job fails if the program asks for a newer glibc (`objdump -T`). GitHub has announced (actions/runner-images issue 14254) that its Ubuntu 22.04 runners are deprecated since September 17th, 2026, fail on purpose during brownouts on March 23, March 30, April 6 and April 13, 2027 (from 14:00 UTC), and are unsupported from April 17th, 2027. Before then this job must move to a container of a system with glibc 2.35 on a newer runner.
- The macOS tool on `macos-15` (Apple silicon, 3 cores).
- Each job has a time limit (`timeout-minutes`), and every action is one of GitHub's own, named by its full commit id with its tag beside it. To update one, resolve the new tag to its commit (`gh api repos/actions/<name>/git/ref/tags/<tag>`) and change both.
