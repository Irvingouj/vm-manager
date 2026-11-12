---
status: accepted
---

# The platform is one Rust binary beside a Postgres

The platform is a single program written in Rust. It contains the host that runs Applications, sign-in and sessions, the admin API, the Dashboard, the Git hosting for Application source, and the command-line tool: the same program serves a company's installation, drives a Builder's deploy, and runs an Application on a Builder's machine in the local development loop. Beside it runs one Postgres instance, supplied by IT or started by the Compose file we ship. That instance holds the platform's own database and one database for each Application.

TypeScript is what Builders and their Coding agents write, so the SDK and the project template are TypeScript packages. The platform itself runs no Node and no TypeScript.

We chose one program because installing has to stay close to "run it" (the PocketBase measure the vision sets), because the execution core is a Rust library and runs natively on Linux, macOS and Windows (`docs/research/execution-core-on-three-platforms.md`), and because one implementation of authorization and audit, used both in an installation and on a Builder's machine, is what makes a local refusal behave like a deployed one.

## Considered Options

- **A Rust host with a TypeScript control plane running as a privileged Application**, the way OpenWorkers runs its own API. It would let us write most of the platform in the language Builders use, but it puts the code that decides authorization inside the sandbox it governs, and makes the platform depend on its own Application runtime to start.
- **A bundle of several services in Compose** (host, API, Git server, dashboard): familiar to operate, further from the install we want, and no help on a Builder's machine.

## Consequences

- Each Application gets a database of its own in the platform's Postgres instance, created at its first deploy, with the two roles of ADR 0013. The platform's role in that instance must be allowed to create databases and roles.
- Registered Resource credentials are stored encrypted in the platform's database, under a key given to the program at install and never stored in the database.
- The program terminates TLS with the wildcard certificate given at install (ADR 0011), or runs behind IT's own reverse proxy.
- The execution core is carried as pinned tags of `openworkers-core` and a vendored, patched `openworkers-runtime-v8` (ADR 0002).
- Builders get the program through an npm package that carries the right build for their machine, so a Coding agent installs it the way it installs any development tool. The agent-facing MCP server is a mode of the same program.
- Git hosting inside one program is not yet proven. The default to try first is bare repositories on disk served over HTTP behind the platform's own sign-in, using the `git` executable shipped in the container image; how far this can go toward a single file is left to the build.
- Server-side builds would need Node on the platform. Milestone 1 has none (ADR 0016).
