---
status: accepted
---

# The manifest is TypeScript and types the Trusted context

An Application's manifest is a TypeScript value in its project, and the same value gives the Trusted context its type. A handler has the Workers shape, `fetch(request, ctx)`, and is registered with `defineApp(manifest, { fetch })`. Through `ctx`, code acts as the User (`ctx.user().db("devops")` for a shared database, `ctx.user().ownDb()` for the Application's own) or as itself (`ctx.app().ownDb()`, its own database only). A database handle is a Drizzle database typed with that database's structure (ADR 0012); one declared at `read` offers no writes.

So the mistakes a Coding agent is likely to make are compile errors with a message that says what to do: naming a Resource the manifest does not declare (the message lists the declared names), writing through a `read` declaration (the message names the manifest line to change), acting as the Application on a shared Resource. No generated file can go stale between the manifest and the types, because there is none.

In Milestone 1 every route of an Application requires sign-in, so `ctx.user()` always returns a User. The shape leaves room for Applications with routes open to people who are not signed in: the manifest would say so, and `ctx.user()` would then be typed as possibly absent.

A refused operation throws one error type, `Refused`, carrying which condition failed (not declared, declared at a lower level, the User holds no Grant, no request in flight, and so on) and a sentence saying how to fix it. Uncaught, it becomes a 403 response.

We chose this from three prototyped shapes (`docs/research/trusted-context-type-shapes.md`).

## Considered Options

- **The vision's sketch with a JSON manifest and a generated type file**: closest to Cloudflare habits, but the generated file goes stale, and its error messages cannot list the declared names.
- **The principal as an argument to every call** (`db("devops").select(ctx.user, ...)`): closest to the vision's wording that Resource calls take the context as a parameter, but noisy, and its error for acting as the Application on a shared Resource misleads.

## Consequences

- The types are guidance for the Coding agent, not the security boundary. The context is a handle and carries no identity; `{} as TrustedContext` compiles and gains nothing, because the host attributes every operation to the request in flight (ADR 0002) and the database role enforces the Access level (ADR 0013).
- The manifest must be a plain literal. Deploy evaluates it on its own and turns it into the Declared access the platform enforces; a manifest assembled from imports or computed values is refused.
- A Platform administrator does not read `manifest.ts`. They read the Declared access the platform recorded from it (ADR 0009).
- The SDK carries generic types that are hard to read in an error message when the whole manifest type is printed.
