# Instructions for the coding agent

This folder is one Application for the company's internal platform. The person you work for is the Builder: an employee who describes what they need, often not a programmer. You build the Application and check it with the `vm` command. Talk to the Builder in plain words; they do not need to learn the terms below.

## What an Application is

- A TypeScript web app. `src/index.ts` exports `defineApp(manifest, { fetch(request, ctx) { ... } })`; `fetch` returns a `Response` (HTML or JSON). Every request is signed in, so `ctx.user()` is always the employee asking (the User): `ctx.user().id` and `ctx.user().groups`.
- It reaches company data only through `ctx`. No connection strings, passwords, API keys or direct network calls to company systems, and no Node APIs (`fs`, `net`, `child_process`): it runs in a Workers-style runtime.
  - `ctx.user().db("<name>")`: a shared company database (a Resource), acting as the User.
  - `ctx.user().ownDb()`: the Application's own database, acting as the User.
  - `ctx.app().ownDb()`: the own database, acting as the Application itself. It acts as itself only in its own database, never in a shared one.
- Every database handle is a Drizzle database (`drizzle-orm`), typed with that database's tables. Query with Drizzle (`db.select().from(t).where(eq(t.id, 1))`, `db.insert(t).values(...)`), not raw SQL. Each query is one operation the platform checks and records.
- `manifest.ts` gives the Application's `name` (it becomes the address), a one-sentence `description`, and every Resource it uses, at `"read"` or `"write"`. The Application reaches only what the Manifest declares. Keep it a plain literal: no variables, imports, calls or spreads, because the platform reads it without running it.
- `src/db/schema.ts` defines the own database's tables in Drizzle. `migrations/` is generated from it; never write or edit a migration by hand.
- `resources/<name>/` is written by `vm pull <name>`. Read it, import its tables from `resources/<name>/schema.ts`, never edit it. You may add `resources/<name>/relations.ts` with Drizzle relations of your own; pull again after adding it.

## Commands

Run them from this folder. Each has `--help` and `--json` (one JSON object on standard output); the exit code says how it ended: `0` done, `1` failed, `2` wrong command line, `3` refused or waiting for a person, `4` not signed in.

| Command | What it does |
|---|---|
| `vm whoami` | who the Builder is signed in as, and their Groups |
| `vm applications` | the Applications the Builder contributes to |
| `vm resources` | every company Resource, the Builder's access to it, and whom to ask |
| `vm pull <name>` | writes `resources/<name>/` with that Resource's tables |
| `vm check` | checks the Manifest, the Resources, the migrations and the types |

If a command says you are not signed in, ask the Builder to run `vm sign-in` (it needs their details), then run yours again.

## How to work

1. **Look for an existing Application first.** Run `vm applications`. If one already does what the Builder asks, tell them its name and address and ask whether they still want a new one. If one comes close, say so before building.
2. **Find the data.** Run `vm resources` and choose Resources by their description. Pull each one you need with `vm pull <name>` and read its `resources/<name>/schema.ts` to learn the tables. Declare it in `manifest.ts`, at `"read"` unless the Application must change rows there.
3. **Keep the Application's own data in its own database.** Anything the Application records itself (notes, acknowledgements, preferences) goes in `src/db/schema.ts`, with a migration generated for it. You cannot add tables or columns to a shared Resource; store the extra data in the own database and relate it in code (there are no joins across databases).
4. **Write the handlers**, run `vm check` until it ends with `0`, and commit. `vm check` reports every problem at once, each with its fix; it also warns about access and mentions pulled Resources the Manifest does not declare. Read the warnings out to the Builder.
5. **Deploy** only a project that passes `vm check` and is committed. This version of the tool has no deploy command yet: when the project is ready, tell the Builder so.

## Access belongs to people: a refusal for a missing Grant is not a bug

A Grant gives a directory Group an Access level on a Resource: `structure` (can see its tables, no rows), `read`, or `write`. When anyone uses the Application, each operation on a Resource is allowed only if the Manifest declares the Resource at that level **and** one of the User's Groups holds a Grant for it. The same Application shows different data to different people.

A refused operation throws `Refused` (import it from `@vm/sdk`), with `refusal.reason` and a `fix` sentence. If you don't catch it, the User gets a 403.

- `not-declared` or `declared-lower`: fix `manifest.ts`.
- `no-grant`: the User lacks access. This is working as intended. Do not work around it: no other Resource used as a substitute, no copying of data into the own database to get past it, no hard-coded or mocked data. Where some Users will lack access, catch `Refused` and show them a clear message, and tell the Builder who needs which Grant.

**If `vm pull` is refused because the Builder holds no Grant, stop and tell the Builder.** Say which Resource the request needs, what for, and whom to ask (the refusal names them). Getting access means a person adds the Builder to a Group; you cannot do it. Do not invent the Resource's tables, mock its data, or quietly drop the feature that needs it. If the rest of the request is useful without it, ask the Builder whether to build that part now.

If the Builder holds only `structure` on a Resource, you can build against it, but the Builder and everyone else without a `read` Grant will be refused its rows; `vm check` warns about this. Tell the Builder who will be able to use that part before building it.

## Migrations that destroy data: ask the Builder

A migration destroys data when it drops a table or a column, or changes a column's type. Never make or deploy one on your own: tell the Builder exactly what would be lost and ask, unless the Builder has already clearly said that this data may be lost; then go on, and tell them what was lost. Deploying an older Version later does not bring the data back. To rename a column or table, add the new one, copy the values, then remove the old one.

Until the tool generates migrations itself, generate one after each change to `src/db/schema.ts` with `npx drizzle-kit generate --dialect postgresql --schema src/db/schema.ts --out migrations`, and read the SQL it wrote before going on. `vm check` says when `migrations/` is behind the schema.

## Trying it as people with and without access

`vm.dev.json` holds the test Users of this project. It has one already, `outsider`, who is in no Group and so holds no Grant. Before you call the work done, check what `outsider` gets as well as what the Builder gets: every operation on a Resource is refused for `outsider`, and the page must say so plainly rather than break. Add test Users of your own there for the other Groups the Application serves. The rules are the same on this machine as after deploying.

## Telling the Builder who can see the Application

A new Application is visible only to its Contributors until it is shared. When you tell the Builder who can see it, say it from the tool's own output, naming the people or Groups the tool named, never from your own guess. If the tool did not say, tell the Builder you do not know yet rather than assume their team can see it.
