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
- `src/db/schema.ts` defines the own database's tables in Drizzle. `migrations/` is generated from it by `vm generate`; never write or edit a migration by hand (the one exception is below).
- `resources/<name>/` is written by `vm pull <name>`. Read it, import its tables from `resources/<name>/schema.ts`, never edit it. You may add `resources/<name>/relations.ts` with Drizzle relations of your own; pull again after adding it.

## Commands

Run them from this folder. Each has `--help` and `--json` (one JSON object on standard output); the exit code says how it ended: `0` done, `1` failed, `2` wrong command line, `3` refused or waiting for a person, `4` not signed in.

| Command | What it does |
|---|---|
| `vm whoami` | who the Builder is signed in as, and their Groups |
| `vm applications` | the Applications the Builder contributes to |
| `vm resources` | every company Resource, the Builder's access to it, and whom to ask |
| `vm pull <name>` | writes `resources/<name>/` with that Resource's tables |
| `vm generate` | writes the next migration from `src/db/schema.ts` into `migrations/`; stops before one that destroys data |
| `vm check` | checks the Manifest, the Resources, the migrations and the types |
| `vm deploy` | checks the committed project again, builds it and puts it live as the Application's next Version |
| `vm versions <name>` | the Application's Versions: commit, when, by whom, and which one serves |
| `vm go-back <name>` | serves the Version before the serving one again, at once (`--to <n>` for an earlier one) |
| `vm logs <name>` | what the Application printed and the errors it did not catch, request by request |
| `vm request <path> --as <user>` | runs the project on this machine and sends it one request as a test User of `vm.dev.json` |
| `vm local operation-log` | the operations of those local requests, and what came of each |
| `vm local change-history` | the changes those local requests made in the own database |
| `vm dev` | keeps the project running on this machine for the Builder's browser, until interrupted |
| `vm local reset` | starts the local databases over: empty, every migration applied again, test rows loaded |

If a command says you are not signed in, run the `vm sign-in` command its message gives. It opens the Builder's browser (or shows an address for the Builder to open, and a check code to type there): tell the Builder so, let them answer in the browser, then run your own command again. If it ends with `sign-in-not-finished`, the Builder has not answered yet: ask them to, then run `vm sign-in` again. Never ask the Builder for a password or a token.

## How to work

1. **Look for an existing Application first.** Run `vm applications`. If one already does what the Builder asks, tell them its name and address and ask whether they still want a new one. If one comes close, say so before building.
2. **Find the data.** Run `vm resources` and choose Resources by their description. Pull each one you need with `vm pull <name>` and read its `resources/<name>/schema.ts` to learn the tables. Declare it in `manifest.ts`, at `"read"` unless the Application must change rows there.
3. **Keep the Application's own data in its own database.** Anything the Application records itself (notes, acknowledgements, preferences) goes in `src/db/schema.ts`; after each change to it, run `vm generate`. You cannot add tables or columns to a shared Resource; store the extra data in the own database and relate it in code (there are no joins across databases).
4. **Write the handlers**, run `vm check` until it ends with `0`, and commit. `vm check` reports every problem at once, each with its fix; it also warns about access and mentions pulled Resources the Manifest does not declare. Read the warnings out to the Builder.
5. **Deploy** with `vm deploy`, once `vm check` passes and everything is committed. The first deploy creates the Application; each deploy puts it live at its address as a new Version, at once (see "Deploying" below).

## Access belongs to people: a refusal for a missing Grant is not a bug

A Grant gives a directory Group an Access level on a Resource: `structure` (can see its tables, no rows), `read`, or `write`. When anyone uses the Application, each operation on a Resource is allowed only if the Manifest declares the Resource at that level **and** one of the User's Groups holds a Grant for it. The same Application shows different data to different people.

A refused operation throws `Refused` (import it from `@vm/sdk`), with `refusal.reason` and a `fix` sentence. If you don't catch it, the User gets a 403.

- `not-declared` or `declared-lower`: fix `manifest.ts`.
- `no-grant`: the User lacks access. This is working as intended. Do not work around it: no other Resource used as a substitute, no copying of data into the own database to get past it, no hard-coded or mocked data. Where some Users will lack access, catch `Refused` and show them a clear message, and tell the Builder who needs which Grant.

**If `vm pull` is refused because the Builder holds no Grant, stop and tell the Builder.** Say which Resource the request needs, what for, and whom to ask (the refusal names them). Getting access means a person adds the Builder to a Group; you cannot do it. Do not invent the Resource's tables, mock its data, or quietly drop the feature that needs it. If the rest of the request is useful without it, ask the Builder whether to build that part now.

If the Builder holds only `structure` on a Resource, you can build against it, but the Builder and everyone else without a `read` Grant will be refused its rows; `vm check` warns about this. Tell the Builder who will be able to use that part before building it.

## Migrations: `vm generate`, and asking before data is lost

After each change to `src/db/schema.ts`, run `vm generate`. Read the SQL it wrote before going on, and commit the three files it names together. `vm check` says when `migrations/` is behind the schema.

A migration destroys data when it drops a table, a column or a schema, or changes a column's type to one that is not wider. `vm generate` stops before writing one (exit code `3`, reason `data-loss`) and says exactly what would be lost. Never confirm it on your own: tell the Builder exactly what would be lost and ask, unless the Builder has already clearly said that this data may be lost; then run `vm generate --allow-data-loss`, and tell them what was lost. `--allow-data-loss` is only for that: never add it on your own otherwise. Deploying an older Version later does not bring the data back.

`vm generate` cannot tell a rename from a removal and an addition (it stops with `rename-not-generated`). To rename a column or table: add the new one beside the old and run `vm generate`; run `vm generate --empty --name <name>` and put in that empty migration the one statement that copies the values (`UPDATE ... SET <new> = <old>;`), the only migration you ever edit, and only before it is committed; then remove the old one and run `vm generate`, which stops for the old one's data as above.

## Trying it as people with and without access

Try the Application before deploying with `vm request <path> --as <user>`: it builds the project as it is now (uncommitted changes too), runs it on this machine and prints the response. Try each page as `builder` (the Builder's own access) and as `outsider` (a test User of `vm.dev.json` in no Group, so every operation on a Resource is refused): the page must say so plainly rather than break. The output also has what the code printed, the errors it did not catch, and each operation's outcome with the reason of a refusal; `vm local operation-log` and `vm local change-history` show what the local requests did.

The Resources there are stand-ins with the real structure and none of the company's rows: a list is empty unless you give it test rows, and a refusal is real. The Grants of `vm.dev.json` decide who may do what; `vm pull` copies the Builder's own Grant there, for the test User `builder`. Add test Users and Grants of your own there for the other Groups the Application serves. The rules are the same on this machine as after deploying.

Test rows are SQL files of the project, never deployed: `test-rows/<name>.sql` for the stand-in of a pulled Resource `<name>`, and `test-rows/own-database.sql` for the own database, mostly `INSERT` statements. Each is loaded when it changes (a stand-in emptied first; the own database made anew first). Run `vm local reset` to start the local databases over.

For the Builder, run `vm dev` (where it can keep running, never as a step that must finish): it serves the project at `http://<name>.localhost:7480/` in their browser, where they choose the test User, and rebuilds it when the project changes. While it runs, your own `vm request` and `vm local` commands are answered by that `vm dev`, as fast as a page.

## Deploying

`vm deploy` sends the project as it is in its last commit, so a deploy needs everything committed: it refuses uncommitted changes and names the files; commit them (or add what is not part of the Application to `.gitignore`) and deploy again. It pulls every declared Resource again: when a shared structure changed, it updates `resources/<name>/` and builds against it (commit those files after), and it fails only if the code no longer type-checks against it. It then makes the checks of `vm check`, and stops on any problem as the check does.

A migration that destroys data is asked about again at deploy when the Application already has a Version, because the rows are there now, even if `vm generate --allow-data-loss` wrote it; it also counts rows you delete in a migration written by hand. Such a migration removes rows with `DELETE`, never with `TRUNCATE`, which the platform refuses. `vm deploy --allow-data-loss` has the same rule as for `vm generate`: tell the Builder exactly what would be lost and ask, unless the Builder has already clearly said that this data may be lost. If a migration fails in the database, nothing is changed and the Version that was serving, if any, goes on serving: read the database's error in the output.

## Debugging, and going back

To debug, read `vm logs <name>`: one entry for each request the code handled, with the User, the Version, what the code printed for it (also while the script was loading) and any error it did not catch, with its stack. Every response carries `x-vm-request-id`; `vm logs <name> --request <id>` shows that request's entry alone. `vm logs <name> --after <next>`, with the `next` of the last run, reads only what came since. `vm logs <name> --follow` prints entries as they come and does not end by itself: run it only where it can be stopped, never as a step that must finish.

`vm go-back <name>` is the quick way out of a bad deploy: the Version before it serves again at once, and nothing is asked. It does not undo migrations: the own database keeps what they made, the older code runs against it, and `migrations/` keeps them, since the next deploy must still carry every deployed migration. `vm versions <name>` shows which Version serves.

## Telling the Builder who can see the Application

A new Application is visible only to its Contributors until it is shared. After a deploy, tell the Builder who can see it from the tool's own output, the deploy's, which names the Contributors who can see it, and never from your own guess; if the tool did not say, tell the Builder you do not know yet rather than assume their team can see it.
