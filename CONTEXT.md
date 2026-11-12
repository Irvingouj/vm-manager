# Vibe Manager (working title)

An open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure.

## Language

### Building blocks

**Application**:
A stateless program, deployed by a Builder, that answers web requests and reaches enterprise infrastructure only through the SDK. It follows the Cloudflare Workers execution model without being Cloudflare-compatible.
_Avoid_: Worker (reserved for future background compute), service, project

**Dashboard**:
The page every employee signs in to, listing the Applications they can contribute to and the Applications shared with them. The same list is available to their agent.
_Avoid_: Portal, console (IT's admin surface), app store

**Integration**:
Separately trusted code that exposes a few narrow business functions and reaches Resources as itself. Applications call it instead of holding privileged access. Anyone may write one; each function takes effect only once the Resource owner releases it.
_Avoid_: Plugin, connector, external API (an outbound HTTP endpoint is a different thing)

**Resource**:
A named piece of enterprise infrastructure that IT registers and governs, and that an Application reaches through the SDK by its logical name alone. Every signed-in employee can see that a Resource exists (its name, a description and whom to ask for access); anything more needs a Grant.
_Avoid_: Binding (the Cloudflare and OpenWorkers term), service, backend

**Resource owner**:
The person or Group answerable for a Resource. They, or a Platform administrator, release the Integration functions that reach it. In Milestone 1 the owner is recorded only; Grants are written by Platform administrators.
_Avoid_: Data owner, steward, admin

**Schema owner**:
The one party allowed to change a database's structure: the Application itself for its own database, and someone outside the Application for a shared one.
_Avoid_: DBA, admin, maintainer

**Trusted context**:
The platform-created value that says which User, Application and request an operation belongs to. Application code receives it and cannot construct one.
_Avoid_: Session, token, credentials

### Authorization

**Grant**:
A Platform administrator's statement that a Group may reach a Resource at a given access level. It names a Group, never a single User, and covers the whole Resource. It holds in every Application, and it also decides who may build against the Resource.
_Avoid_: Permission, ACL, role

**Access level**:
How much of a Resource a Grant opens. For a database there are three, each including the one before: structure (pull its table structure, no rows), read, and write.
_Avoid_: Role, permission set, scope

**Group**:
A set of Users defined in the enterprise directory and reported by the identity provider. The platform does not define its own. Grants are given to Groups.
_Avoid_: Role, team, access list

**Declared access**:
The list, fixed in an Application's manifest at deploy, of the Resources it uses and at what access level. Nothing outside it is reachable.
_Avoid_: Binding, scope, app permission

**Autonomous grant**:
A Grant that lets code reach a Resource as itself, whatever the User it is serving may access. Only an Integration can hold one on a shared Resource; an Application acts as itself only in its own database.
_Avoid_: Service account, elevated access, bypass

### Audit

**Operation log**:
The platform's record of every operation on a Resource: who, through which Application, what statement, how many rows, and whether it succeeded. It contains none of the Resource's data.
_Avoid_: Audit trail (ambiguous between the two layers), access log

**Change history**:
The record, kept beside the data, of each row that was changed: old value, new value, who and through which Application. Readable by whoever may read the data.
_Avoid_: Audit log, versioning, revision table

### People

**Platform administrator**:
A member of the one directory Group named when the platform is installed. They register Resources, write Grants, see every Application and can disable any of them. The platform keeps no administrator accounts of its own.
_Avoid_: Superuser, root, IT (when this specific role is meant)

**Builder**:
An employee who creates and deploys an Application, normally by directing a Coding agent. Any signed-in employee may create one.
_Avoid_: Developer, author, employee (when the building role is meant)

**Contributor**:
A person or Group allowed to change and deploy a given Application. An Application has no owner: it belongs to the company, and what is controlled is who may contribute to it.
_Avoid_: Owner, maintainer, author

**User**:
An employee who signs in to a deployed Application and uses it.
_Avoid_: End user, customer, employee (when the using role is meant)

**Coding agent**:
An AI tool that writes Application code at a Builder's direction.
_Avoid_: AI, bot, agent (unqualified)
