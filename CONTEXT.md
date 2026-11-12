# Vibe Manager (working title)

An open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure.

## Language

### Building blocks

**Application**:
A stateless program, deployed by a Builder, that answers web requests and reaches enterprise infrastructure only through the SDK. It follows the Cloudflare Workers execution model without being Cloudflare-compatible.
_Avoid_: Worker (reserved for future background compute), service, project

**Resource**:
A named piece of enterprise infrastructure that IT registers and governs, and that an Application reaches through the SDK by its logical name alone.
_Avoid_: Binding (the Cloudflare and OpenWorkers term), service, backend

**Trusted context**:
The platform-created value that says which User, Application and request an operation belongs to. Application code receives it and cannot construct one.
_Avoid_: Session, token, credentials

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
