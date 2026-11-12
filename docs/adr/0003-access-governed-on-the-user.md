# Access is governed on the User; an Application reaches only what it declares

Governance binds to identity, in the model enterprise IT already runs on: directory groups. When IT registers a Resource it chooses which Groups may reach it and at what access level. Groups come from the identity provider; the platform defines none of its own. That answer holds in every Application: a User outside those Groups cannot reach the Resource through any of them, so the Resource is protected no matter what Applications exist.

An Application is not approved or restricted resource by resource. It must declare, statically in its manifest, which Resources it uses and at what access level, and the platform refuses anything outside that Declared access at runtime. A deploy never waits for a person.

So an operation on behalf of a User is allowed when the User holds a Grant and the Application declared the Resource. There is no third check on which Applications may use a Resource.

The same Grant governs building. A Builder can discover a Resource and pull its structure only if they hold a Grant on it, so a Builder without access finds out at the first step, when their Coding agent tries to pull the structure, and can ask IT then, instead of after the Application is built.

An Application never acts as itself on a shared Resource. The only thing it reaches as itself is its own database. Anything that must go beyond the User's own access is done by calling an Integration, which is Milestone 2.

## What this defends against

We assume Application code is unreliable but not hostile: it is AI-generated and carries npm dependencies, yet it broadly does what its Builder intended. Declared access therefore guards against accident and against a poisoned dependency widening an Application's reach at runtime, since the manifest is fixed at deploy. A Resource is never read directly by someone outside its Groups. What is not prevented is an Application copying what an authorized User read into its own database, where others may later see it; that is visible in the manifest, recorded in the audit trail and attributable to the Builder.

## Considered Options

- **Check the User only, with no declaration**: nobody can read what an Application touches, and a dependency can reach anything the visiting User can.
- **IT approves each Application's access before it goes live**: moves governance from a runtime decision to a pre-release gate, and makes IT's workload grow with the number of Applications.
- **A per-Resource list of the Applications allowed to use it**: a second axis of governance beside identity. Rejected as unnecessary once the Resource is protected by its Groups; it can be added later without changing anything else.

## Consequences

- IT's work is the work it already does: deciding which Groups reach which Resource.
- Who may open an Application carries no privilege and is its Builder's call, defaulting to the Builder alone.
- A refusal must say which condition failed (no Grant, not declared), so that a Coding agent can tell a missing declaration from a bug.
- Because a deploy pulls the structure again to type-check, a Builder who has lost their Grant on a Resource can no longer deploy an Application that declares it.
