# Vibe Manager (working title)

An open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure.

## Language

### Building blocks

**Application**:
A stateless program, deployed by a Builder, that answers web requests and reaches enterprise infrastructure only through the SDK. It follows the Cloudflare Workers execution model without being Cloudflare-compatible.
_Avoid_: Worker (reserved for future background compute), service, project

**Integration**:
Separately trusted code that exposes a few narrow business functions and reaches Resources as itself. Applications call it instead of holding privileged access; each of its functions is a Resource.
_Avoid_: Plugin, connector, external API (an outbound HTTP endpoint is a different thing)

**Resource**:
A named piece of enterprise infrastructure that IT registers and governs, and that an Application reaches through the SDK by its logical name alone.
_Avoid_: Binding (the Cloudflare and OpenWorkers term), service, backend

**Schema owner**:
The one party allowed to change a database's structure: the Application itself for its own database, and someone outside the Application for a shared one.
_Avoid_: DBA, admin, maintainer

**Trusted context**:
The platform-created value that says which User, Application and request an operation belongs to. Application code receives it and cannot construct one.
_Avoid_: Session, token, credentials

### Authorization

**Grant**:
IT's statement that a User, a group or an Application may reach a Resource at a given access level.
_Avoid_: Permission, ACL, role

**Delegated grant**:
A Grant that lets an Application reach a Resource on behalf of the User it is serving. It only takes effect where that User also holds a Grant on the same Resource.
_Avoid_: App permission, on-behalf-of token

**Autonomous grant**:
A Grant that lets code reach a Resource as itself, whatever the User it is serving may access. Only an Integration can hold one on a shared Resource; an Application acts as itself only in its own database.
_Avoid_: Service account, elevated access, bypass

### People

**Builder**:
An employee who creates and deploys an Application, normally by directing a Coding agent.
_Avoid_: Developer, author, employee (when the building role is meant)

**User**:
An employee who signs in to a deployed Application and uses it.
_Avoid_: End user, customer, employee (when the using role is meant)

**Coding agent**:
An AI tool that writes Application code at a Builder's direction.
_Avoid_: AI, bot, agent (unqualified)
