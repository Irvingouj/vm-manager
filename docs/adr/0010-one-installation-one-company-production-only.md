---
status: accepted
---

# One installation is one company, and a deployed Application has one environment

An installation serves exactly one company. There is no notion of an organization or tenant anywhere in the platform: not in the Trusted context, not in Grants, not in the data model. A group with subsidiaries that must be kept apart installs the platform once per subsidiary.

A deployed Application has a single environment, production. The only other place an Application runs is the Builder's machine, in the fully local development loop, which never reaches a real Resource. There are no preview or staging deployments in Milestone 1.

The product is self-hosted software for one enterprise's internal Applications. Multi-tenancy is a concern of someone hosting the platform for others, which is not what we are building, and carrying an organization through every Grant and every operation would complicate the one thing that must stay simple to reason about: who can reach what.

## Considered Options

- **Carry an organization from the start**, so that a hosted, multi-company offering needs no migration later. Rejected: it taxes every Milestone 1 design for a product we have not decided to build.
- **A preview deployment per Application** in Milestone 1. Deferred: it raises the question of which Resources a preview may reach, which needs an answer of its own. It is on the roadmap.

## Consequences

- Nothing is reserved in the Milestone 1 data model for organizations or for further environments. Adding either later is a migration we accept.
- With no staging, a Builder's change goes straight from their machine to the Users of the Application. Going back to the previous deployed version has to be quick and easy; this is for Application manifest, bundling and deploy lifecycle to settle.
