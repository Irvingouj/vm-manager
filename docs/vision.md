# Vision: An Enterprise Backend Platform for AI-Generated Applications

## The Vision

Build an **open-source, self-hosted, enterprise-grade application platform** that allows employees and AI coding agents to create, deploy, and operate applications within a shared, IT-managed environment.

Think **PocketBase for the enterprise**, designed for the AI coding era.

IT configures the environment once. Employees can then build applications using preconfigured enterprise services without implementing authentication, managing credentials, provisioning infrastructure, or building integrations from scratch.

The platform provides a unified, type-safe SDK that makes secure resource access the default development experience.

## 1. A Shared Enterprise Environment

Instead of every application managing its own infrastructure, the organization maintains shared resources:

- **Authentication:** Enterprise SSO and user identities.
- **Databases:** Shared and application-specific PostgreSQL databases.
- **Storage:** Shared, named storage resources.
- **Secrets:** Centrally managed credentials.
- **Integrations:** Reusable, governed business functions and external APIs.
- **Audit:** Centralized records of resource operations.
- **Compute:** Managed application execution.

Resources are registered and managed by IT, then made available to authorized applications through a consistent SDK.

```typescript
ctx.db("devops")
ctx.storage("marketing")
ctx.integration("finance")
ctx.httpClient("salesforce")
```

Applications refer to logical resource names without needing to know their physical locations, credentials, or underlying infrastructure.

## 2. A Type-Safe, Identity-Aware SDK

The SDK is the primary interface between applications and enterprise infrastructure.

Every resource operation requires a trusted execution context.

```typescript
ctx.user().db("devops").execute(sql);

ctx.user().storage("marketing").get("report.pdf");

ctx.user().httpClient("salesforce").get("/customers");

ctx.user().integration("finance").reimburse(input);
```

The context represents the authenticated user, executing application, organization, and request.

**Application code cannot independently construct a valid trusted context.**

The platform creates and injects the context. Its type is intentionally opaque, and resource APIs require it as a parameter.

This guides AI-generated code toward correct identity propagation while runtime enforcement prevents forged contexts from gaining authorization.

The same approach applies to service and agent identities, with explicit distinctions between user-initiated and autonomous operations.

## 3. Centralized Authorization and Audit

IT controls which applications and users can access which resources.

Every resource operation carries identity context, allowing the platform to record:

- Who initiated the request.
- Which application executed it.
- Which resource was accessed.
- What operation was performed.
- Whether it succeeded or failed.
- When it happened.

Applications share infrastructure without automatically sharing permissions.

The platform provides resource-level access control and can support finer-grained policies.

Business-specific authorization remains the responsibility of the relevant business logic or trusted integration.

## 4. Governed Business Integrations

Organizations can expose approved business capabilities without exposing their underlying infrastructure.

For example, an ordinary employee may have no permission to access the finance database directly but may be authorized to submit a reimbursement.

```typescript
ctx.user().integration("finance").reimburse({
  amount: 150,
  description: "Travel expenses"
});
```

The `finance.reimburse` integration executes trusted business logic using its own restricted access to the finance database.

The calling application cannot obtain its credentials or execute arbitrary SQL against that database.

IT can approve integrations at the individual function level.

These integrations form a reusable catalog of enterprise capabilities that AI agents can discover and compose into new applications.

## 5. AI-Native Application Development

The platform is designed for applications written by both humans and AI coding agents.

Through the SDK and agent-facing tooling, AI agents can discover:

- Available enterprise resources.
- Authorized integrations and operations.
- Resource schemas and TypeScript types.
- Required permissions.
- Application runtime conventions.

An employee can describe a business application to an AI coding agent, which builds it using existing enterprise infrastructure and approved capabilities.

The application inherits the platform's authentication, resource access, authorization, and auditing mechanisms.

**AI generates application logic. The enterprise platform supplies the governed environment.**

## 6. TypeScript-First, Stateless Application Runtime

The initial platform focuses exclusively on TypeScript applications.

Applications follow a Cloudflare Workers-style execution model:

- Stateless compute.
- Platform-provided request contexts.
- Managed access to persistent resources.
- No application-managed infrastructure credentials.
- Independent application execution and deployment.

Background workers and other compute capabilities eventually follow the same stateless execution model.

The SDK and application programming model remain consistent regardless of where the application executes.

## 7. Single-Machine First, Distributed Later

The platform begins as a complete self-hosted system running on one machine.

Its initial environment consists of:

- One application management platform.
- One PostgreSQL instance hosting multiple databases.
- Shared storage and enterprise resources.
- Isolated TypeScript application runtimes.
- A centralized identity, authorization, and audit system.

The deployment experience should be as straightforward as installing PocketBase.

As organizational usage grows, the architecture evolves to support multiple machines, distributed application execution, and independently hosted resources.

**Applications continue using the same SDK and logical resource names as the infrastructure evolves.**

## 8. Enterprise IT as the Platform Owner

IT manages the shared environment rather than individually configuring every generated application.

Its responsibilities include:

- Enterprise identity configuration.
- Resource registration and provisioning.
- Application permissions.
- Integration approval and management.
- Secrets and credential management.
- Audit and operational oversight.
- Platform maintenance.

Employees create applications using the infrastructure and capabilities IT has already authorized.

The goal is to enable widespread application creation without requiring every employee to become an infrastructure or security engineer.

## Long-Term Vision

As AI coding advances, organizations may accumulate hundreds or thousands of internally generated applications.

The platform becomes the **shared enterprise backend and runtime** supporting those applications.

Every application operates within a common environment of identities, resources, permissions, and integrations.

Every approved business integration becomes a reusable capability for future applications.

Every application can be managed, audited, and operated through the same enterprise platform.

The ambition is to make enterprise application development as accessible as building with PocketBase, while preserving centralized IT governance and organizational control.

**IT configures once. Humans and AI build freely within authorized boundaries. The platform manages the shared infrastructure, identities, permissions, and audit trail.**
