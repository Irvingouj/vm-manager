---
status: accepted, pending the Identity pass-through prototype
---

# Our own host on the OpenWorkers execution core

Applications run in V8 isolates provided by the execution core of [OpenWorkers](https://github.com/openworkers): the `openworkers-core` and `openworkers-runtime-v8` crates. We take only that core. Everything around it is ours: ingress, sign-in, the Trusted context, authorization, the Operation log, database execution, deploy and routing. We do not use or fork the OpenWorkers runner, API or dashboard.

Sign-in, the Trusted context, authorization and audit are enforced in the host, so a User's identity never enters an Application's JavaScript heap as something that could be copied or replayed. The host is Rust; how much of the rest of the platform is TypeScript is decided in Platform layout and install form.

We chose it after reading the source (`docs/research/openworkers-host-layer.md`). Every outward call a guest makes goes to one host-side handler trait whose methods all have defaults, and `openworkers-embed` is an existing host of about 3,000 lines built on the runtime's public API alone, so the boundary is real. The runner, by contrast, is a product: one runner per platform database, bound to OpenWorkers' own schema, with guest SQL running under the same Postgres role as its internal pool. An earlier version of this decision was a thin patch on the runner; the source does not support that.

## Security posture for Milestone 1

- Who may publish an Application is strictly internal: Builders are authenticated, accountable employees.
- What the code contains is unreliable but assumed not hostile: it is AI-generated and carries npm dependencies, yet it broadly does what its Builder intended. We defend against accident and against a dependency overreaching, not against a Builder acting in bad faith.
- A V8 sandbox escape is an accepted risk. It is mitigated by running the host in a tightly restricted container: read-only filesystem, no writable volumes, network limited to the ingress port and Postgres.

## Considered Options

- **A thin patch on the OpenWorkers runner**: the previous decision. Rejected on the evidence above.
- **Fork the runner and rewrite it freely**: the fallback if building the host from the core proves slower than expected.
- **Build on unmodified OpenWorkers, all TypeScript**: identity as a short-lived signed token handed through Application code; a bearer secret inside code we do not fully trust.
- **workerd**: the fallback runtime if the Identity pass-through prototype fails. Its expected advantages (Cloudflare-maintained V8 patching, Node compatibility, capability-passing RPC) are not verified.

## Consequences

- The host process holds the credentials of every registered Resource and can reach Postgres, so a V8 escape exposes them regardless of filesystem restrictions. Splitting the credential-holding broker into its own process is roadmap work.
- The core has one maintainer and a pre-1.0 API that changes quickly. We pin tags and expect to carry V8 upgrades ourselves if upstream stops.
- A guest is a single classic script with no Node APIs, so an Application must be bundled into that shape.
- Several crates the core depends on have no LICENSE file although their manifests say MIT; this needs clearing with the maintainer before release.
- OpenWorkers' docs position it for running your own code, not untrusted third-party code. Our posture is consistent with that and must be revisited if who may publish ever widens.
