---
status: accepted, pending the Identity pass-through prototype
---

# Thin fork of the OpenWorkers runner, with identity enforced in the host

Applications run in V8 isolates on a thin fork of the [OpenWorkers runner](https://github.com/openworkers/openworkers-runner) (Rust). Sign-in, the Trusted context, authorization and audit are enforced in the runner's host operations layer, so a User's identity never enters an Application's JavaScript heap as something that could be copied or replayed. Rust is confined to that trust boundary; the control plane, admin API, CLI, SDK and type generation are TypeScript.

We chose it because OpenWorkers already provides most of the plumbing Milestone 1 needs (isolate runtime, ingress, deploy, a validating Postgres proxy, single-machine Compose deployment), and every outward call a worker makes already passes through one host-side handler, which is the seam identity needs.

## Security posture for Milestone 1

- Who may publish an Application is strictly internal: Builders are authenticated, accountable employees.
- What the code contains is unreliable but assumed not hostile: it is AI-generated and carries npm dependencies, yet it broadly does what its Builder intended. We defend against accident and against a dependency overreaching, not against a Builder acting in bad faith.
- A V8 sandbox escape is an accepted risk. It is mitigated by running the runner in a tightly restricted container: read-only filesystem, no writable volumes, network limited to the ingress port and Postgres.

## Considered Options

- **Build on unmodified OpenWorkers, all TypeScript**: gateway and resource broker as privileged workers, identity as a short-lived signed token handed through Application code. No fork, but the token is a bearer secret inside code we only semi-trust. Kept as the fallback if we do not want to own Rust.
- **Fork the whole OpenWorkers platform (runner, API, dashboard)**: fastest start, but inherits a data model where users own their resources, the opposite of IT-governed Resources.
- **workerd**: the fallback runtime if the Identity pass-through prototype shows the host seam is unworkable. Expected advantages (Cloudflare-maintained V8 patching, Node compatibility, capability-passing RPC) were not verified when this was written; none of OpenWorkers' platform plumbing exists there.

## Consequences

- The runner process holds the credentials of every registered Resource and can reach Postgres, so a V8 escape exposes them regardless of filesystem restrictions. Splitting the credential-holding broker into its own process is roadmap work.
- We carry V8 security patching ourselves if upstream stalls. Upstream currently has a single maintainer and moves fast.
- The upstream docs position OpenWorkers for running your own code, not untrusted third-party code. Our posture above is consistent with that, and must be revisited if who may publish ever widens.
