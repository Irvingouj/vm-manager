# A private Postgres on the Builder's machine, without Docker: findings

Date: 2026-11-12 (UTC). Code: branch `spike/local-postgres`, under `prototypes/local-postgres/`. Feeds the map ticket "Local dev loop".

Provenance: this is the report of the engineering agent that did the work, saved as returned. The driving session independently checked:

- [CI run 43163614664](https://github.com/Irvingouj/vm-manager/actions/runs/43163614664) at commit `a506251`: all six jobs (ubuntu, macos, windows × two distributions) succeeded. In its log (`/tmp/lp-ci.log`): the runs as the standard users (`lpuser`, with `BUILTIN\Users` and Medium Mandatory Level on Windows, "NOT a member of admin" on macOS); the forged-identity check passing on Windows; `getaddrinfo` failing for every `*.localhost` on Windows while Chrome and Edge load `http://my-app.localhost:<port>/` there; the Windows path cases, including the 8.3 short path working under `C:\Users` and failing under `D:`.
- A local run of `prototypes/local-postgres/run.sh` on a fresh checkout (Ubuntu 22.04): 48 PASS, 0 FAIL, `RUN: PASS`, Postgres 18.6, download 14.4 MiB, first start 104 ms (log `/tmp/lp-verify-run.log`); no Postgres process or directory left afterwards.

Not re-checked: the comparison of candidates and its sources, the Linux distribution matrix, the PGlite probes, the measurements table beyond the lines above, and the agent's judgments.

## 1. Verdict

**Yes, with one limit on Windows.** The platform program can give a Builder a private, real Postgres 18 with no Docker, no admin rights and no manual install. It downloads prebuilt relocatable binaries into a per-user directory, runs `initdb`, and starts the server with `pg_ctl` on a free 127.0.0.1 port. It ran green this way on Linux, macOS and Windows in CI, both as the runner account and as a freshly created standard local user.

**Use zonky's Postgres builds from Maven Central** (`io.zonky.test.postgres:embedded-postgres-binaries-<platform>`):

- Downloaded by fixed URL and checked against Maven's `.sha1`, never through the GitHub API.
- Driven by `initdb`/`pg_ctl`. In the spike that goes through the `postgresql_embedded` crate; a few dozen lines of our own would do the same (the agent's judgment).

**What it asks of the Builder's machine:** about 15–70 MB of download once, about 60–320 MB of disk, outbound HTTPS to the download host, and one loopback port. Only on Windows: the data and binary paths must be ASCII.

**Main risks:**

- **Windows non-ASCII paths (verified in CI).** Postgres on Windows fails `initdb` when the path has characters outside ASCII, and that includes a user profile like `C:\Users\Zoë\…`. A workaround exists but isn't guaranteed (section 4).
- **Endpoint protection on managed Windows machines.** The binaries are unsigned. Whether security tools block them is untested: the CI runner has real-time protection off.
- **Supply chain.** The best-behaved distribution is a third-party "for testing" bundle. The agent's judgment: we should mirror or re-host the archives we pin.

## 2. Candidates compared

| Candidate | Licence / maintainer | Platforms (Postgres 18) | Download (PG 18.6) | Verdict |
|---|---|---|---|---|
| **zonky embedded-postgres-binaries** 18.6.0 | Apache-2.0 packaging, PostgreSQL licence; zonkyio, active (last push 2026-08-26) | linux amd64/arm64, darwin amd64/arm64 (one universal binary for both), windows amd64; no Windows ARM | linux-amd64 14.4 MiB, windows-amd64 24.2 MiB, darwin 67.9 MiB | **Best.** Linux build runs on glibc 2.31 to 2.44. Ships only `initdb`, `pg_ctl`, `postgres`. Describes itself as "for testing purposes" and bundles `libcrypto.so.1.1`. |
| **theseus-rs postgresql-binaries** 18.6.1 | PostgreSQL licence; theseus-rs, active (last push 2026-10-28) | Many Linux targets, macOS x64/arm64, Windows x64 and (new in 18.6.1) arm64 | linux-gnu 38.6 MiB, windows 46.4 MiB, macOS arm64 84.7 MiB | Works on Windows, macOS and recent Linux. Linux build needs **glibc ≥ 2.36**: fails on Ubuntu 22.04, Debian 11, Rocky 9. |
| theseus 18.6.0 | same | same | linux 11.6 MiB, windows 51.6 MiB | Linux build depends on system `libssl.so.3`, `libgssapi_krb5.so.2` and `libxml2.so.2`. It failed on the ubuntu-latest runner (Ubuntu 26.04) and on most container images. |
| `postgresql_embedded` crate 0.22.0 (+ `postgresql_archive`) | Apache-2.0 OR MIT; theseus-rs | Wraps theseus (default) or zonky | — | Useful for `initdb`/`pg_ctl`/stop. Its default download path calls `api.github.com`, which is rate-limited per IP; it returned **403 on a macOS runner**. Default command timeout is 5 s. |
| EDB zip binaries | PostgreSQL licence; EnterpriseDB | Windows x86-64 and macOS only, no Linux | not measured | No Linux, so not a full answer. |
| **PGlite** 0.5.16 (Node) + pglite-socket 0.2.19 | Apache-2.0; README marks itself alpha | Needs a JS runtime | npm unpacked 26 MB | **Rejected (see notes).** |
| pglite-oxide (Rust, WASM) | MIT/Apache/PostgreSQL; one maintainer | — | — | **Rejected (see notes).** |
| "Use the Builder's own Postgres" | — | — | — | Not tested. Needs a superuser able to create roles and databases, a version we support, and setup we can't assume of a non-engineer. Judgment: keep only as an optional override. |

**PGlite, what the agent ran (Node probe):**

- Supported: roles, `SET ROLE` privilege denial, SECURITY DEFINER, plpgsql triggers, read-only transactions, `CREATE DATABASE`.
- Over the wire through pglite-socket:
  - Logging in as role `ro` gave `current_user = postgres`, and an INSERT the role isn't granted succeeded. The startup user is ignored, so the ADR 0013 per-login boundary cannot exist.
  - A second client blocked for more than 5 s while the first held an open transaction.
- Needs Node, which ADR 0015 rules out for the platform.

**pglite-oxide, what the agent ran:**

- 0.5.1 does not compile: the alpha wasmer-wasix dependency fails with E0004.
- 0.3.0 runs Postgres 17.5. The connection dropped on `CREATE FUNCTION … LANGUAGE plpgsql`, and a second connection was refused or reset.

**Sources for the table:** crates.io API; GitHub API for both repos and their releases; Maven metadata and HEAD sizes; https://pglite.dev/docs/pglite-socket (states the core is single-connection and uses a multiplexer); https://www.enterprisedb.com/download-postgresql-binaries; the `postgresql_archive` 0.22.0 source (`repository/github/repository.rs` calls `api.github.com`).

**Linux distro matrix (the agent ran it in Docker, as a non-root user, `initdb` + start + stop):**

| Image (glibc) | zonky 18.6.0 | theseus 18.6.0 | theseus 18.6.1 |
|---|---|---|---|
| ubuntu 20.04 (2.31) | OK | missing libssl.so.3 | needs glibc 2.33 |
| ubuntu 22.04 (2.35) | OK | "command not found" (not investigated) | needs glibc 2.36 |
| ubuntu 24.04 (2.39) | OK | missing libgssapi_krb5 | OK |
| ubuntu 26.04 (2.43) | OK | missing libgssapi_krb5 | OK |
| debian 11 (2.31) | OK | missing libssl.so.3 | needs glibc 2.33 |
| debian 13 (2.41) | OK | missing libgssapi_krb5 | OK |
| fedora latest (2.44) | OK | missing libssl.so.3 | OK |
| rockylinux 9 (2.34) | OK | OK | needs glibc 2.35 |

Minimal container images lack libraries a real desktop has, so the theseus 18.6.0 failures overstate the problem on real laptops (judgment). The ubuntu-latest runner failure was real.

## 3. Per platform: final green run

Run: https://github.com/Irvingouj/vm-manager/actions/runs/43163614664 at commit `a506251`, all 6 jobs green. Each job runs:

- Two sessions: a cold one (download, extract, initdb, start, checks, measure, stop), then a warm one (start on the existing cluster, checks, stop).
- The same two sessions again as a non-admin user.
- Headless browsers, and on Windows the path-name cases.

The checks all passed in every session:

- **Access levels:**
  - The read login can read but not write (`42501`), and cannot see a schema it wasn't granted.
  - The write login inside `BEGIN READ ONLY` is refused (`25006`).
  - A shared-database login cannot connect to the Application's database.
- **Identity:**
  - `vm.attach` and `vm.identity()` work (SECURITY DEFINER).
  - A forged `vm.ctx` is refused, with no history row.
  - A guessed secret is refused.
- **Change history:**
  - The trigger records the insert.
  - The runtime role cannot DISABLE TRIGGER, DROP TRIGGER, set `session_replication_role`, or delete history.
  - Control case: a table the runtime role owns *can* have its trigger disabled.
- **Other:**
  - Two databases exist; 20 concurrent connections each complete their own transaction.
  - Data survives stop/start; `listen_addresses=127.0.0.1`; no `postmaster.pid` is left after stop.

**Measurements (all from CI run 43163614664):**

| Job | Download (size / time) | Extract | Install disk | initdb | First start | Later start | Stop | Data disk | Idle RSS: sum of 9 processes / postmaster |
|---|---|---|---|---|---|---|---|---|---|
| ubuntu 26.04 x64, zonky | 14.4 MiB / 209 ms | 603 ms | 56.5 MiB | 1519 ms | 105 ms | 105 ms | 101 ms | 70.3 MiB | 87.6 / 25.9 MiB |
| ubuntu, theseus 18.6.1 | 38.6 MiB / 683 ms | 457 ms | 109.8 MiB | 530 ms | 107 ms | 107 ms | 101 ms | 70.5 MiB | 111.0 / 28.8 MiB |
| macOS 26.7.2 arm64, zonky | 67.9 MiB / 412 ms | 4969 ms | 321.4 MiB | 829 ms | 146 ms | 133 ms | 136–175 ms | 71.0 MiB | 70.8 / 26.5 MiB |
| macOS, theseus 18.6.1 | 84.7 MiB / 1148 ms | 1083 ms | 251.1 MiB | 525 ms | 182 ms | 193 ms | 128–178 ms | 71.0 MiB | 65.0 / 21.1 MiB |
| Windows Server 2025 x64, zonky | 24.2 MiB / 353 ms | 1772 ms | 104.4 MiB | 1306 ms | 268 ms | 247 ms | 133–137 ms | 72.2 MiB | 137.9 / 26.2 MiB |
| Windows, theseus 18.6.1 | 46.4 MiB / 865 ms | 790 ms | 119.2 MiB | 1432 ms | 171 ms | 155 ms | 132 ms | 72.3 MiB | 129.7 / 25.2 MiB |

Notes on the measurements:

- Download times are on GitHub datacenter networks; an office link will be slower.
- The RSS sum counts shared buffers once per process, so it overstates real use.
- Windows `initdb` as the non-admin user took 4.9–5.4 s, against 1.3–1.4 s as the administrator account. The cause wasn't investigated.
- Paths: on Linux and macOS the data and binary path was `…/lp spike Zoë 数据` and everything passed. On Windows the main runs used `…\lp spike Zoe` (ASCII with a space); see section 4.

## 4. Findings

### Non-administrator accounts

- **Linux (CI):** a new user made with `useradd`, no sudo, using `~/.local/share/lp spike Zoë 数据`. All checks passed.
- **macOS (CI):** a new standard user made with `sysadminctl` ("lpuser is NOT a member of admin", no sudo). All checks passed.
  - No quarantine attribute was set on the downloaded binaries. Gatekeeper's `spctl` reports "rejected", but they ran.
  - theseus binaries are ad-hoc signed; the zonky universal binary carries a non-ad-hoc signature. The signer was not inspected.
- **Windows (CI):**
  - The runner account is an administrator, and Postgres ran fine there. Postgres drops admin rights itself.
  - A local user `lpuser` was then created, a member of **Users only**: `BUILTIN\Users`, Medium Mandatory Level, only SeChangeNotify and SeIncreaseWorkingSet privileges.
  - The program was run as that user via `Start-Process -Credential -LoadUserProfile`, with data in `C:\Users\lpuser\AppData\Local\…`. All checks passed in both sessions.
  - One gotcha: that process inherited the administrator's environment (`LOCALAPPDATA`), so the first attempt failed with "Access is denied". The product should find per-user folders through the OS API, not environment variables (judgment).
  - **Still unverified for a real office account:** a domain/Entra-joined, Intune-managed laptop with Defender for Endpoint, AppLocker or WDAC, or third-party EDR.

### Windows path names (CI, both distributions, ANSI code page 1252)

| Root | Result |
|---|---|
| ASCII with a space | works |
| `Zoë` | `initdb` gets to post-bootstrap, then `FATAL: invalid byte sequence for encoding "UTF8": 0xeb …` |
| `数据` (CJK) | `invalid binary "…??…\initdb"` |
| CJK data directory, ASCII binaries | fails: `could not open file "…?? data\superuser-password"` |
| 8.3 short name of a CJK directory under `C:\Users\…\AppData\Local` | **works** |
| Same under `D:` | fails: that volume generates no 8.3 names |

So the product needs one of:

- the short-path trick, which works only where 8.3 names exist;
- an ASCII fallback location;
- verifying that Windows' UTF-8 code-page setting fixes it (not tested).

### Office-machine obstacles

- **Antivirus (evidence and gaps):**
  - Defender on the runner: service and antivirus enabled, but **real-time protection and behaviour monitoring off**, 0 detections. CI therefore proves nothing about AV reactions.
  - Both Windows builds are **not signed** (Authenticode), with no Mark-of-the-Web stream.
  - Microsoft documents an attack-surface-reduction rule, "Block executable files from running unless they meet a prevalence, age, or trusted list criterion", off unless an admin configures it. Where it is enabled, it is the kind of control that would block an unsigned, rarely-seen `postgres.exe` (https://learn.microsoft.com/en-us/defender-endpoint/attack-surface-reduction-rules-reference). That is the agent's reading, not tested.
  - Judgment: plan to ship our own Authenticode-signed builds, or ones signed by EDB, plus an allow-list note for IT.
- **Proxies and download hosts:**
  - Downloads use plain HTTPS to `repo1.maven.org` (zonky) or `github.com` release assets (theseus).
  - The crate's default GitHub-API lookup hit a 403 rate limit in CI; behind a corporate NAT this would be worse (judgment).
  - Proxy handling and custom root CAs were **not tested**. reqwest honours `HTTPS_PROXY`, but a rustls build would not trust a TLS-inspecting corporate CA unless it is added (from code and judgment, not run).
- **Bundling into the npm package:** per-platform packages would add the archive size per platform: zonky 14.4 (linux-x64), 24.2 (win-x64), 67.9 MiB (mac, universal). That is feasible with per-platform optional packages (judgment; npm size limits not checked).
- **Ports:** a free 127.0.0.1 port is picked each start and Postgres listens only there (verified). Whether the Windows Firewall prompt appears for a loopback-only listener on a real desktop is not verified.

### `*.localhost`

- **OS resolver (`getaddrinfo` from Rust std), CI:**
  - macOS: resolves to ::1 and 127.0.0.1.
  - Ubuntu runner: resolves (through the systemd-resolved stub).
  - **Windows: "No such host is known" (11001) for every `*.localhost`; only `localhost` itself resolves.**
  - In containers: glibc without systemd-resolved does **not** resolve it (debian:13, ubuntu:26.04). Fedora does, through `nss-myhostname`.
- **curl:** worked on all three. curl special-cases `*.localhost` itself, so this doesn't show the OS resolves it.
- **Headless browsers (CI):**
  - **Chrome 156 and Edge 156 loaded `http://my-app.localhost:<port>/` on Windows and macOS; Chrome did on Ubuntu**, including on Windows where the OS resolver fails, so the browser resolves it itself.
  - Ubuntu's Chromium crashed and Edge refused to run without its sandbox (runner setup, unrelated).
  - Firefox and Safari were not run.
- **Conclusion:** `http://<application>.localhost:<port>` works in Chrome and Edge everywhere. Any non-browser client of ours (CLI, Node tooling) must not rely on the Windows OS resolver; it should connect to 127.0.0.1 and send a `Host` header.

## 5. What was built

Branch `spike/local-postgres`, pushed to origin. Commits: `2b55f84`, `b0f4563`, `afa4658`, `41e535d`, `2ae1b54`, `a506251`.

- `prototypes/local-postgres/src/main.rs` builds the `lp` binary:
  - `lp pg <root> [version] [zonky|theseus]` runs one session; `lp localhost` runs the resolver and HTTP checks.
  - SQL is copied from `spike/postgres-access` (`sql/cluster.sql`, `app_db.sql`, `shared_db.sql`).
- `run.sh` is the one command.
- `ci/browser.sh`, `ci/nonadmin-unix.sh`, `ci/nonadmin-windows.ps1`, `ci/windows-paths.sh`.
- Workflow `.github/workflows/local-postgres.yml`: ubuntu, macos and windows × zonky 18.6.0 and theseus 18.6.1.

**The one command:** `prototypes/local-postgres/run.sh`. It builds, runs a cold and a warm session in a fresh directory whose path has a space and non-ASCII characters, runs the `*.localhost` checks and curl, prints METRIC/PASS/FAIL and `RUN: PASS|FAIL`, then deletes the directory.

**Versions:** Rust 1.99.1; `postgresql_embedded` and `postgresql_archive` 0.22.0; tokio-postgres 0.7.18; reqwest 0.13.5. The server reports "PostgreSQL 18.6" on every platform. Runners: Ubuntu 26.04.1 (glibc 2.43), macOS 26.7.2 arm64, Windows Server 2025.

## 6. Still unknown

- **Real managed Windows laptop:**
  - Defender with real-time protection, attack-surface-reduction rules or other EDR reacting to unsigned `postgres.exe`/`initdb.exe` started from `%LOCALAPPDATA%`; AppLocker/WDAC.
  - The firewall prompt.
  - Whether Windows' UTF-8 code-page setting fixes non-ASCII paths; how common machines without 8.3 names are on C:.
- **Corporate proxies and TLS inspection:** not tested.
- **Untested architectures:** Linux arm64, macOS x86-64, Windows ARM64 (zonky has no Windows ARM build).
- **Long-term trust in a "for testing" third-party bundle:** zonky's bundled OpenSSL 1.1 and its update cadence. Whether to build and sign our own archives is a decision, not tested.
- **Product-level details:** recovering a stale `postmaster.pid` after a crash or reboot; two program instances sharing one cluster; upgrading the Postgres major version under an existing data directory.
- **`postgresql_embedded`'s settings:** it initialises with `--auth=password` and picks its own port by binding 0.0.0.0 (bypassed in the spike). Whether to keep the crate or call `initdb`/`pg_ctl` directly is open.
- **Firefox and Safari** on `*.localhost`: not run.
