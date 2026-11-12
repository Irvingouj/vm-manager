# Access is governed on the User; an Application reaches only what it declares

Governance binds to identity, in the model enterprise IT already runs on: directory groups. When IT registers a Resource it chooses which Groups may reach it and at what access level. Groups come from the identity provider; the platform defines none of its own. That answer holds in every Application: a User outside those Groups cannot reach the Resource through any of them. An Application is not approved resource by resource. It must declare, statically in its manifest, which Resources it uses and at what access level, and the platform refuses anything outside that Declared access at runtime. A deploy never waits for a person. For the Resources IT marks as restricted, only the Applications IT has listed may use them; for every other Resource the declaration is enough.

So an operation on behalf of a User is allowed when the User holds a Grant, the Application declared the Resource, and the Resource is either unrestricted or lists the Application.

An Application never acts as itself on a shared Resource. The only thing it reaches as itself is its own database. Anything that must go beyond the User's own access is done by calling an Integration, which is Milestone 2.

## What this defends against

We assume Application code is unreliable but not hostile: it is AI-generated and carries npm dependencies, yet it broadly does what its Builder intended. Declared access therefore guards against accident and against a poisoned dependency widening an Application's reach at runtime, since the manifest is fixed at deploy. It does not stop a Builder who deliberately declares more than the Application needs. That is visible in the manifest, recorded in the audit trail and attributable to the Builder, and on restricted Resources IT's list stops it.

## Considered Options

- **Check the User only, with no declaration**: an Application silently inherits the full access of whoever opens it, and nobody can read what an Application touches.
- **IT approves each Application's access before it goes live**: moves governance from a runtime decision to a pre-release gate, and makes IT's workload grow with the number of Applications.
- **Cap an Application's access at its Builder's own**: blocks the ordinary case of someone building a tool for a team whose data they cannot see themselves.

## Consequences

- IT's work scales with the number of sensitive Resources, not with the number of Applications.
- Who may open an Application carries no privilege and is its Builder's call, defaulting to the Builder alone.
- A refusal must say which condition failed (no Grant, not declared, not listed), so that a Coding agent can tell a missing declaration from a bug.
