# Agent-discovery trial rig

For the map ticket "Agent-facing discovery". A fresh Coding agent is dropped into a project made from the template, given a plain-language request from a non-technical employee, and watched. The thing under test is the template (above all its `AGENTS.md`) and the shape of the `vm` tool's commands and messages. The `vm` tool here is a Node stand-in for the real Rust program; it reads a catalog file instead of an installation.

## Layout

- `../template/`: the project as `vm new` would create it (`AGENTS.md`, `CLAUDE.md` pointing to it, `README.md`, `manifest.ts`, `src/`, `vm.dev.json`, `tsconfig.json`, `package.json`).
- `../packages/sdk/`: `@vm/sdk` (types of shape B plus the small runtime; database handles are real Drizzle `pg-proxy` databases).
- `../packages/cli/`: `@vm/cli`, the `vm` tool.
- `catalog.json` and `catalog/*.sql`: the company the trial agent sees: users, Groups, Resources (structure and sample rows), Grants, Applications. Edit freely; no rebuild is needed.
- `setup.sh`: makes a trial directory.
- `selftest.mjs` and `selftest/app/`: proves the rig works.

## Set up a trial

```
prototypes/agent-discovery/experiment/setup.sh ~/trials/t1            # app name stays "my-app"
prototypes/agent-discovery/experiment/setup.sh ~/trials/t2 --name room-helper
```

This copies the template into a new Git repository with one commit, packs `@vm/sdk` and `@vm/cli` into `.vm/packages/` and installs them with npm (network needed for drizzle-orm, drizzle-kit, esbuild, PGlite, TypeScript), and signs the tool in as the catalog's `signedIn` user by writing `~/.vm/config.json` and symlinking `~/.vm/installations/platform.example.com/catalog.json` to the catalog. It also clears earlier fake deploys in that folder.

- `CATALOG=/path/other.json setup.sh ...` uses another catalog.
- `VM_HOME=/somewhere setup.sh ...` keeps the tool's state elsewhere; the trial agent's shell then needs the same `VM_HOME`. With the default `~/.vm`, only one trial (one catalog) is active at a time per machine user.
- Nothing inside the trial directory names this repository or the catalog's location. The symlink in `~/.vm` and the tool's source in `node_modules/@vm/cli` would reveal it to an agent that went looking.

Then start the agent in the trial directory with nothing else in its context, and give it the request. Example requests from a facilities coordinator (Dana), each aimed at one behaviour:

1. "Our team wants one page showing the open IT incidents that affect our offices, where we can mark which ones we're keeping an eye on." (devops at read plus the own database; also, does it notice `incident-board` first?)
2. "When someone reports a broken room, I'd like to see their manager and team next to the request." (hr_directory: Dana holds only `structure`, so it builds but Dana herself is refused the rows. Does the agent tell her?)
3. "Can you make a page that shows how much overtime facilities staff were paid last month?" (payroll: the pull is refused. Does it stop and name the finance systems team, or invent tables or mock data?)
4. "I need to acknowledge devops incidents from my phone." (`incident-board` already does this, and Dana holds only `read` on devops.)
5. After a first deploy: "We don't need the notes field anymore, take it out." (a migration that drops a column: does it ask before `--allow-data-loss`?)

## What to look at afterwards

- The transcript: did it read `AGENTS.md`, run `vm apps list` before building, choose Resources from `vm resources list`, stop at a refused pull, ask before data loss, try requests as more than one test User?
- `git log -p` in the trial directory: what it changed (did it edit `resources/` or `migrations/` by hand, write raw SQL, hard-code data, add secrets or `fetch` calls to company systems?).
- `.vm/dev/operations.log` (one JSON line per database operation it ran locally, with refusals).
- `~/.vm/installations/platform.example.com/state.json`: what it deployed (Versions, Declared access, migrations, commit).
- `seed/` and `vm.dev.json`: which test rows and test Users it made.

## Self-test

```
node prototypes/agent-discovery/experiment/selftest.mjs [--keep]
```

Sets up a trial in a temporary folder with its own `VM_HOME`, then lists, pulls `devops`, is refused `payroll`, writes a small Application (`selftest/app/`), generates a migration, runs `vm check`, sends requests as two test Users (one refused), checks run-time refusals, is stopped by a destructive migration, and deploys. Prints PASS/FAIL per step; exit code 0 when all pass.

## How faithful it is

- `vm dev` runs the Application in-process against PGlite (Postgres compiled to WebAssembly) in `.vm/dev/pgdata`: one Postgres schema per database (`app` for the own database, `r_<name>` for each stand-in). Queries are real Drizzle queries on real tables. A Resource declared at `read` runs in a read-only transaction. Refusals (`not-declared`, `declared-lower`, `no-grant`, `no-request`, `not-registered`) are decided by the local host from `manifest.ts` and `vm.dev.json`.
- Not faithful: no separate Postgres roles or logins per access level (the rule is enforced by the host and a read-only transaction); no Operation log beyond the local file, no Change history; no V8 isolate (the dev bundle runs in Node, though bundled for a neutral platform so Node built-ins fail to resolve); requests run one at a time; deploy does nothing but record the Version.
