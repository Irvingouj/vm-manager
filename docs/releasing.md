# Making a release

A release is the image on GitHub's container registry and a GitHub release with the Compose files and the tool for Linux and macOS (`blackbox/CONTRACT.md`, "The first release"). It is made by the workflow `.github/workflows/release.yml` from a commit of `main` whose local gate was green. Nothing is published by hand.

## Before the tag

1. Choose the version, `<version>` below (for example `0.2.0`).
2. Change it in every place that names it:
   - `crates/vibe/Cargo.toml` (the package `vibe`), then `cargo build` so that `Cargo.lock` follows;
   - the `Dockerfile`: `ARG VIBE_VERSION=<version>`;
   - `deploy/compose.yaml` (`${VIBE_IMAGE:-ghcr.io/irvingouj/vibe:<version>}`) and `deploy/.env.example`;
   - `docs/getting-started.md`: every address `.../releases/download/v<version>/...` and every archive `vibe-<version>-<target>.tar.gz`.
3. Write the release's notes in `docs/releases/v<version>.md`: what works, what is missing (the open issues, in a user's words), and what was and was not tried.
4. Run `scripts/release-check.sh`. It names each place that does not agree, and says when `LICENSE` is missing. Run it until it passes.
5. Run the gate (`scripts/gate.sh`) and the install gate (`scripts/install-gate.sh`). Both must be green.
6. Commit, and push to `main`.

## A rehearsal

Start the workflow by hand on `main` (on GitHub: Actions, "release", "Run workflow"; or `gh workflow run release.yml --ref main`). It runs every job but `publish`: the release check (without a tag, so a missing `LICENSE` is said and does not fail it), the gate, the image and the install tests, the tool built, packed and tried on Linux and on macOS, and the release's files. Its files are kept for 7 days as the run's artefacts: `image` (the image the install tests passed, as `docker save` writes it), `tool-<target>` (each archive), and `release-files` (everything the release would hold, with `SHA256SUMS`).

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

Fix the cause, and run the workflow again for the same tag, by hand ("Run workflow", and choose the tag `v<version>` instead of a branch). A draft left by the failed run is replaced. If the fix needs a new commit, the tag must move to it: delete the tag (`git push origin :refs/tags/v<version>` and `git tag -d v<version>`), and tag the new commit of `main`.

The first time an image is published, GitHub makes its package private. The run then stops after pushing the image, before the release is made, saying so. Make the package public once (on GitHub: your profile, Packages, `vibe`, Package settings, Change visibility, Public), and run the workflow again for the tag.

## What the workflow runs on

- `check`, `gate`, `image`, `files` and `publish` on `ubuntu-24.04`. The gate and the image jobs first remove large tools the repository does not use (Android, .NET, GHC, CodeQL, Swift), so that the debug build fits on the disk, and stop if less than 25 GB is free.
- The gate job runs `scripts/gate-hosted.sh`, not `scripts/gate.sh` directly: the whole gate once with `--no-fail-fast`, then each test file that failed once more, alone and one test at a time (`cargo test --locked <target> -- --test-threads=1`, the target as cargo names it in its "to rerun pass" lines). The gate's time limits are sized for a 12-core machine; the runner has 4, and in the rehearsals six tests of four files that start many programs at once ran past their limits side by side, each passing alone. The job fails if a step before the tests fails, if npm's tests fail, if cargo's tests fail without naming a file, or if a file fails both times. Its log ends with each file run again and how it ended, so a release's log says what did not pass side by side.
- Before the gate, the job allows unprivileged user namespaces (`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`, as Ubuntu 24.04's release notes give it) and checks that the runner's user can make a network namespace with only the loopback (`unshare -rn ...`, the command `blackbox/tests/local_offline.rs` runs). Ubuntu 24.04 restricts them by default through AppArmor, and that test makes one; without the setting it fails with "write failed /proc/self/uid_map: Operation not permitted". The setting lasts until the runner, which is thrown away after the job, stops.
- The Linux tool on `ubuntu-22.04`, whose glibc is 2.35: the job fails if the program asks for a newer glibc (`objdump -T`). GitHub has announced (actions/runner-images issue 14254) that its Ubuntu 22.04 runners are deprecated since September 17th, 2026, fail on purpose during brownouts on March 23, March 30, April 6 and April 13, 2027 (from 14:00 UTC), and are unsupported from April 17th, 2027. Before then this job must move to a container of a system with glibc 2.35 on a newer runner.
- The macOS tool on `macos-15` (Apple silicon, 3 cores).
- Each job has a time limit (`timeout-minutes`), and every action is one of GitHub's own, named by its full commit id with its tag beside it. To update one, resolve the new tag to its commit (`gh api repos/actions/<name>/git/ref/tags/<tag>`) and change both.
