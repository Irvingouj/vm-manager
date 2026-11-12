---
status: accepted
---

# A deploy is built on the Contributor's machine and becomes an immutable Version

Deploying is an explicit act by a Contributor, separate from pushing source. The command-line tool, on the Contributor's machine, pulls the current structure of every shared database the Application declares, type-checks the Application against it, evaluates the manifest, bundles the code into the single script the runtime needs, and sends the platform a Version: the bundle, the Declared access, the pending migrations for the Application's own database, and the Git commit it was built from, which it pushes to the Application's repository in the same step. The platform applies the migrations, and only if they succeed does it switch the Application's address to the new Version. Declared access changes with the switch.

Versions are immutable and kept. Going back is one command that switches the address to the previous Version at once. With no staging environment (ADR 0010), this is what makes a bad deploy cheap.

The platform does not build. It runs no Node (ADR 0015), and building means running a project's dependencies' install and build scripts, which we do not want to do on the machine that holds every Resource's credentials.

## Considered Options

- **A push deploys, and the platform builds from Git**: the familiar hosted experience, and the deployed code provably matches the source. Deferred to the roadmap for the reason above.
- **Rolling the database back together with the code**: not offered. Migrations only go forward.

## Consequences

- The platform trusts that a bundle was built from the commit it names. This is within the assumption that Builders are not hostile (ADR 0002); the Declared access does not depend on that trust, because it is enforced at run time whatever the bundle contains.
- Going back to an earlier Version does not undo a migration. A migration that destroys data therefore needs the Contributor's explicit confirmation at deploy, and the tool names what will be lost; a Coding agent must put that question to the Builder.
- A migration that fails leaves the database unchanged and the previous Version serving.
- Deleting an Application frees its address at once and keeps its database and repository for a retention period, during which a Platform administrator can restore them.
- Deploy needs the Contributor to hold a Grant on every Resource the Application declares, since it pulls their structure (ADR 0003).
