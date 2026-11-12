# Roadmap after Milestone 1

Headline level only. Each milestone has one outcome a company could be shown. The order is a judgment, made to be revised once Milestone 1 has been used; nothing here is specified.

Milestone 1 itself is described by the map issue ("Map: the way to a build-ready Milestone 1 spec") and the ADRs in `docs/adr/`.

## Vocabulary settled for what follows

- **Integration**: separately trusted code exposing a few narrow business functions, released by a Resource owner (ADR 0008). It is not an outbound API.
- **HTTP client**: a kind of Resource. A named external HTTP endpoint that a Platform administrator registers together with its credentials, reached through the SDK by name. The vision's "integrations with external APIs" are HTTP clients.
- **Secret**: a kind of Resource. A named value that code may use and nobody may read back. A registered database's login is not a Secret; it is part of that Resource and never reaches code.
- **Cache** and **Storage** (files) are kinds of Resource, governed by Grants like any other.

## Milestone 2: Integrations

*An Application does something its User could not do directly, through a function the Resource's owner released.*

- Integrations: anyone writes one; each function is released per Version by the Resource owner or a Platform administrator; Applications call released functions through the SDK.
- Resource owners become real: how a Resource gets one, what the owner is shown when asked to release a function, and whether owners also manage the Grants on their Resource.
- Autonomous grants, held only by Integrations.
- HTTP clients and Secrets, because most Integrations exist to reach something outside. Outbound access becomes per Application and per Integration instead of one platform-wide list.

## Milestone 3: IT's console and deeper shared databases

*IT governs from a web page, and a shared database can trust who is asking.*

- The admin web console over the existing admin API: Resources, Grants, every Application with its Declared access and Contributors, the Operation log, disabling.
- The platform's identity schema installable in a shared database by its Schema owner. That brings Change history there, and lets the owner write row-level rules that trust the platform's User identity (ADR 0013).
- Row rules for an Application's own database, declared by the Application and enforced by the platform.
- Dashboard extensions: search, grouping and ordering set by IT.

## Milestone 4: more kinds of Resource

*Applications keep files, share data with each other and stop re-solving the same problems.*

- Storage and Cache.
- One Application's database offered to other Applications as a Resource, by its Contributors, under Grants.
- Platform-managed structure for shared databases: a reviewed schema project as the Schema owner.

## Milestone 5: beyond the signed-in request

*Applications work when nobody is looking, and can face people outside the company.*

- Scheduled and background work (Workers), and what identity such work runs under.
- Routes that need no sign-in.
- Applications calling other Applications.

## Milestone 6: hardening and scale

*The platform is ready for a company that depends on it.*

- The credential-holding broker split into its own process, so a sandbox escape does not reach every Resource's credentials (ADR 0002).
- Builds on the platform from the Git source, so deployed code provably matches its source (ADR 0016).
- Preview deployments (ADR 0010).
- More than one machine.

## What Milestone 1 leaves room for

| Later capability | What Milestone 1 already does so that it fits |
|---|---|
| Other kinds of Resource | Every Resource and every Manifest entry has a kind; Milestone 1 has one kind, a Postgres database. |
| Integrations and Autonomous grants | Operations are attributed to a principal that has a kind; Milestone 1 has two kinds, the User and the Application itself. The Operation log records the kind. |
| Resource owners | A Resource records its owner from the start, though the owner can do nothing yet. |
| Routes without sign-in, background work | The SDK's types can express "no User" (ADR 0014); the host already refuses an operation that has no request and no identity. |
| Identity in shared databases | The identity schema used in Application-owned databases is the one a shared database's owner would install. |
| Per-Application outbound rules | Outbound calls already pass through the host with the Application and User attached. |
| Builds on the platform, previews | A deploy is already an immutable Version tied to a Git commit. |
| A separate credential broker | Only the host touches Resource credentials; Application code never holds one. |

Not planned: compatibility with Cloudflare's bindings (ADR 0001), multi-tenancy (ADR 0010), and a platform-run Coding agent or builder web page (ADR 0006).
