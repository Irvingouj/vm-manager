# The outside of the `vm` program

What the black-box tests assume about the program. Tests are written from this file and the tickets' acceptance criteria; the implementation conforms to it. A change to the outside is made here first.

Each section says which ticket introduced it. Words in capitals are from `CONTEXT.md`.

## How the tests get the program

The harness runs `cargo build -p vm --bin vm` once and starts `target/debug/vm`, unless the environment variable `VM_BIN` names a binary to use instead. `blackbox` depends on no product crate.

## How the tests get a Postgres

The harness runs a private Postgres of its own, never through the program under test: from the binaries in the directory `VM_TEST_PG_BIN` names (`postgres`, `initdb`, `pg_ctl`), else from a Postgres found on the machine, else from prebuilt binaries it downloads once to a cache (`docs/research/local-postgres-without-docker.md`). No Docker, no administrator rights. `VM_TEST_PG_VERSION` (16, 17 or 18; default 18) chooses the version it downloads, so the whole suite can be run against each. It hands the program a superuser's connection address. Each test's program gets a platform database of its own name on that instance.

## Serving (ticket: Walking skeleton)

### Starting and stopping

```
VM_DATABASE_URL=postgres://<role>:<password>@<host>:<port>/<platform database> \
vm serve --domain <domain> --listen <ip:port> [--stand-in-sign-in] [--administrators-group <group>]
```

- `--domain` is the installation's domain. `--listen 127.0.0.1:0` picks a free port.
- `--administrators-group` names the Group whose members are Platform administrators. Without it nobody is one.
- `VM_DATABASE_URL`, in the environment and never on the command line, names the platform database and a role that may create databases and roles. See "The platform database".
- A start-up that cannot work exits non-zero, within 15 seconds, with a message on standard error naming the problem: no `VM_DATABASE_URL`, one that cannot be parsed, a Postgres that cannot be reached or refuses the role, a platform database written by a newer program. The message never shows the password.
- (Ticket: Walking skeleton named fixture Applications with `--application <name>=<path>`. That option is gone: Applications enter through the platform's API.)
- The program writes its log to standard error, one JSON object per line, each with `"event"` and `"time_ms"`.
- When it is ready to serve it writes `{"event":"listening","address":"127.0.0.1:<port>","stand_in_sign_in":<bool>, ...}`.
- It is stopped by ending the process. Nothing is promised about requests in flight at that moment.

### Addresses

- A request whose `Host` is `<application>.<domain>` goes to that Application's serving Version. Letter case and a port in `Host` are ignored.
- `api.<domain>` is the platform's API. The names `api`, `auth`, `dashboard`, `git`, `admin` and `www` are the platform's own and can never be an Application's.
- Tests connect to the listening address and set `Host`.

### Stand-in sign-in

A development-only way for a caller to state who the User is. Real sessions replace it later without the rest of this file changing.

- It does nothing unless the program was started with `--stand-in-sign-in`.
- `x-vm-stand-in-user: <id>` states the User, exactly once. `x-vm-stand-in-groups: g1,g2` states their Groups; optional, at most once; entries are trimmed, empty ones dropped, repeats kept once.
- Values are UTF-8 and are handed over unchanged; a User id need not be ASCII. A value of 256 bytes is accepted.
- Malformed (two User headers, two Groups headers, an empty User, control characters, bytes that are not UTF-8, a value longer than 256 bytes): `400`. When the stand-in is off these headers are not looked at, so they are never malformed.
- Groups keep the order in which they were first stated.
- Every request header whose name starts with `x-vm-`, in any letter case, is removed before the request reaches Application code, whether the stand-in is on or off.

### Responses the program itself gives

Checked in this order:

| Condition | Status |
|---|---|
| No `Host` | 400 |
| Sign-in malformed | 400 |
| Not signed in (this includes every request when the stand-in is off) | 401 |
| `Host` is not `<application>.<domain>` for a known Application, or the Application has no serving Version | 404 |
| The Application threw, rejected, answered with something that is not a `Response`, or failed while loading | 500 |

- Sign-in is checked before the Application is looked up, so someone who is not signed in cannot learn which Applications exist.
- These bodies are `text/plain`, between 1 and 200 bytes. The `401` body is the same whatever was asked for. The `500` body is fixed: it holds neither the error's message nor its stack.
- Every response carries `x-vm-request-id`, unique per request.

### What the log records per request

| `event` | Fields |
|---|---|
| `printed` | `request`, `application`, `user`, `level`, `message` |
| `uncaught` | `request`, `application`, `user`, `error` (message and stack) |
| `host-call` | `request` (or null), `application`, `user` (or null), `operation`, `outcome` (`allowed` or `refused`), `reason` (when refused) |
| `request` | `request`, `application` (or null), `version` (or null), `user` (or null), `method`, `path`, `status` |

- `request` in these records is the value of `x-vm-request-id`. `user` is the User's id. `level` is the name of the console method. `path` is the path without the query.
- For a `400` or `401`, `application` and `user` are null. For a `404`, `user` is the signed-in User's id and `application` is null.
- The `request` record is written last for its request, before the response is sent: no record carrying a request's id comes after it, also when the Application left calls unawaited. A `host-call` record is written when the host decides, and an allowed one always names its request.
- A `printed` record from code that runs after its request ended, or while a script loads, has a null `request`.
- "No Application code ran" for a request means: no `printed`, `uncaught` or `host-call` record carries its id.

### What an Application is handed

- An Application's code is its serving Version's bundle: a classic script that registers its handler by assigning `globalThis.default = { fetch(request, ctx) { ... } }`. `fetch` may be async and returns a `Response`.
- Every request gets a fresh context. The script's top level runs again for each request, before the handler, **with no request in flight**. Nothing in memory survives from one request to another. The program may also run a script's top level when it starts, equally with no request in flight.
- `request` is a standard `Request`: the method, `http://<host><path-and-query>`, the headers without any `x-vm-*`, and the body.
- `ctx` is frozen. `ctx.user()` returns a frozen `{ id: string, groups: string[] }`: the User of the request in flight, as the host knows it. Called when the request is over, it throws an `Error` whose message starts with `no request in flight`.
- What the Application prints with `console.log` / `info` / `warn` / `error` / `debug` appears as `printed` records.

### The raw host interface

The SDK wraps this later; Application authors never write it by hand. Fixture Applications use it directly.

- `env.VM.invoke(operation, args)` returns a Promise of either `{ ok: <value> }` or `{ refused: { refusal: { reason: <string>, ... }, fix: <string> } }`. It does not reject for a refusal.
- The host decides who is asking from the request in flight, read on entry. Nothing the Application passes, sets or overwrites changes that.
- An operation may also fail without being refused: `{ error: { kind: <string>, ... } }`. See "Operations on a shared database".
- Operation `whoami` (no arguments) answers `{ ok: { user: { id, groups }, application: <name>, version: <number>, request: <request id> } }`.
- Called with no request in flight (at the top level while loading, or from a timer or a kept reference after the request ended): refused with reason `no-request`. Code left running after its request may also simply never run; what must not happen is a call that is allowed.
- An operation the host does not know: refused with reason `unknown-operation`.
- Outbound `fetch` from Application code is refused (it rejects) until the ticket on outbound calls.

### Refusal reasons

Kebab-case strings, the same everywhere a refusal is reported (to Application code, in the log, later in the Operation log and in the tool's output). Known so far: `no-request`, `unknown-operation`, `not-own-database`, `not-declared`, `not-registered`, `declared-lower`, `no-grant`, `log-unavailable`.

## The platform database (ticket: Applications and their Versions live in the platform database)

- The program keeps everything it must remember in the platform database named by `VM_DATABASE_URL`. If that database does not exist the program creates it. It creates its tables at first start and brings an older platform database up to date at start, in place.
- Stopping the program and starting it again against the same platform database loses nothing: Applications, Contributors, Versions and which Version is serving are as they were.
- Two tests' programs on the same Postgres instance with different platform databases do not see each other.

## The platform's API (ticket: Applications and their Versions live in the platform database)

Served at `api.<domain>`; letter case and a port in `Host` are ignored, as for Applications. Requests and answers are JSON. The caller is a signed-in User, by the same sign-in as everywhere (for now the stand-in). The checks for a missing `Host`, a malformed sign-in and not being signed in are the ones in "Responses the program itself gives", with their plain-text bodies, and come before everything below, whatever the request's path or body. Requests to the API have `request` records like any other, with `application` and `version` null.

An answer that is not a success is:

```json
{ "error": { "reason": "<kebab-case>", "message": "<a sentence a Coding agent can act on>" } }
```

| Status | When | `reason` |
|---|---|---|
| 400 | The body is not the JSON asked for | `malformed` |
| 400 | An Application name that is not a lower-case DNS label of 1 to 63 characters (letters, digits and hyphens; it may start with a digit; not starting or ending with a hyphen), or that starts with `xn--` | `invalid-name` |
| 400 | An Application name that is exactly one of the platform's own (`apis` or `my-api` are fine) | `reserved-name` |
| 403 | The User is not a Contributor of the Application | `not-a-contributor` |
| 403 | The request is for Platform administrators and the User is not one | `not-an-administrator` |
| 404 | No such Application, Version or path | `not-found` |
| 405 | A path the API has, with a method it does not have there | `method-not-allowed` |
| 409 | The Application's name is taken | `name-taken` |
| 409 | A Version's migrations do not continue the Application's earlier ones | `migrations-diverged` |
| 413 | A body larger than 16 MiB (exactly 16 MiB is accepted) | `too-large` |
| 422 | A Version's migration failed in the database | `migration-failed` |
| 408 | The body did not arrive in time | `timed-out` |
| 503 | The platform database cannot be reached or did not answer in time | `unavailable` |

When several apply, the first of these wins: `too-large`, `not-an-administrator`, `not-found`, `method-not-allowed`, `not-a-contributor`, `malformed`, then the rest. `not-an-administrator` comes before `not-found` so that someone who is not a Platform administrator learns nothing from an administrators' path. So someone who is not a Contributor learns nothing about whether their body was well formed.

- A body with a missing or wrongly typed field, a key given twice, or JSON that does not parse is `malformed`. Fields the request does not have (also ones the platform sets, like `created_by` or `number`) are ignored.

### Applications

`POST /v1/applications` with `{ "name": "<name>", "description": "<text, optional>" }`. Any signed-in User may. Answers `201` with the Application:

```json
{ "name": "incident-board", "description": "...", "address": "incident-board.<domain>",
  "contributors": [ { "user": "<user id>" } ], "serving_version": null,
  "created_by": "<user id>", "created_at": "<RFC 3339 time>" }
```

The creator is its first Contributor. A `description` of `null` is the same as none. Names are unique in the installation. `description` is null when none was given; `serving_version` is a number, or null before the first Version.

`GET /v1/applications/<name>` answers `200` with the same object. In this ticket every signed-in User may read it; who may see an Application is settled by the ticket on Contributors and sharing.

### Versions

`POST /v1/applications/<name>/versions`, by a Contributor, with:

```json
{ "bundle": "<the classic script, as text>",
  "declared_access": { "resources": { "<name>": { "kind": "postgres", "access": "read" } } },
  "git_commit": "<40 hex characters>" }
```

All three fields are required. `declared_access.resources` may be an empty object; `kind` is `postgres`; `access` is `structure`, `read` or `write`. `bundle` may be any text, also empty: what does not run is still a Version. Answers `201` with the Version, which from then on is the Application's serving Version:

```json
{ "application": "incident-board", "number": 1, "declared_access": { ... }, "git_commit": "...",
  "bundle_sha256": "<hex>", "created_by": "<user id>", "created_at": "<RFC 3339 time>" }
```

- Versions of an Application are numbered 1, 2, 3, ... in the order they were sent. `bundle_sha256` is the SHA-256 of the bundle's UTF-8 bytes.
- A Version is immutable: `GET /v1/applications/<name>/versions/<number>` answers the same object for ever, whatever is sent later. Which Version serves is a fact about the Application (`serving_version`), not about the Version.
- `GET /v1/applications/<name>/versions` answers `{ "versions": [ ... ] }`, oldest first.
- Once the `201` has been received, every new request to the Application is served by the new Version. Requests already in flight finish on the Version they started on.
- A bundle that fails when it runs is still a Version; its requests get the fixed `500`.

## Resources and Grants (ticket: A User's read of a shared database is authorized and logged)

On the platform's API. Writing is for Platform administrators only; everyone else gets `403 not-an-administrator`.

`POST /v1/resources`, by a Platform administrator:

```json
{ "name": "devops", "kind": "postgres", "description": "...", "owner": "...", "contact": "whom to ask for access",
  "connection": { "host": "...", "port": 5432, "database": "...",
                  "read": { "role": "...", "password": "..." },
                  "write": { "role": "...", "password": "..." } } }
```

`connection.write` is optional (ticket: Write access, and the database role as the boundary). The platform installs nothing in a shared database and never changes role in it: what each login may do there is the Platform administrator's to set with ordinary Postgres privileges.

- A Resource's name is 1 to 63 characters: lower-case letters, digits and underscores, starting with a letter. Otherwise `400 invalid-name`. A taken name is `409 name-taken`. `kind` is `postgres`.
- `name`, `kind`, `description`, `owner`, `contact` and `connection` with its `read` login are required.
- Answers `201` with `{ "name", "kind", "description", "owner", "contact", "created_by", "created_at" }`.
- **No answer of the API, no log record and no error message ever holds a Resource's password**, and none holds its host, port, database or role either.
- Registering does not connect to the database (the ticket on Resources in full adds that check).

`GET /v1/resources/<name>` and `GET /v1/resources` (`{ "resources": [ ... ] }`, by name): the same object, for every signed-in User. A Resource's existence, description and whom to ask are visible to all; nothing else is.

`POST /v1/resources/<name>/grants`, by a Platform administrator, with `{ "group": "<Group>", "access": "structure" | "read" | "write" }`: sets that Group's Grant on the Resource, replacing the level if the Group already had one. Answers `200` with `{ "resource", "group", "access" }`. A Group is 1 to 256 bytes with no control characters, compared exactly; anything else, or an unknown `access`, is `400 malformed`.

`GET /v1/resources/<name>/grants`, by a Platform administrator: `{ "grants": [ { "group", "access" } ] }`, by Group.

A Grant holds in every Application: nothing is set up per Application.

## Operations on a shared database (same ticket)

Through the raw host interface:

```js
env.VM.invoke("query", { resource: "devops", sql: "select id, title from incidents where open = $1", params: [true] })
```

- `sql` is one statement. `params` is optional; each is `null`, a string, a number or a boolean, and is sent to Postgres as text for the server to interpret.
- The operation needs Access level `read`. It runs in a read-only transaction as the Resource's read login, so a statement that writes fails in the database.
- Answer when it ran: `{ ok: { columns: ["id", "title"], rows: [[1, "..."], ...], row_count: 2 } }`. A value is `null`, a boolean, a number (for `int2`, `int4`, `float4`, `float8`), parsed JSON (for `json`, `jsonb`), and otherwise the text Postgres prints for it (so `int8`, `numeric`, timestamps and `uuid` are strings). The SDK ticket may refine this.
- Answer when Postgres refused or failed the statement: `{ error: { kind: "database", code: "<SQLSTATE>", message: "<Postgres's message>" } }`.
- A result of more than 10,000 rows or 8 MiB, or a statement running longer than 10 seconds, is ended: `{ error: { kind: "limit", limit: "rows" | "bytes" | "time" } }`. (The ticket on limits makes these settable.)
- Malformed arguments (arguments that are not an object; `resource` or `sql` missing or not a string; `params` not an array; a param that is an object or an array): `{ error: { kind: "malformed", message } }`, and nothing reaches any database. Arguments the operation does not have are ignored and never take effect.
- A shared database that cannot be reached or refuses the Resource's login: `{ error: { kind: "unavailable" } }`, with nothing about the connection in it.
- The program does not change the session's `TimeZone` or `DateStyle`: values print as the shared database is set to print them.

### Who is allowed

Decided by the host from the request in flight, the serving Version's Declared access, and the Grants of the User's Groups; never from anything the Application passes. Checked in this order, the first that applies refuses:

| Reason | When | Also in `refusal` |
|---|---|---|
| `no-request` | No request in flight | |
| `not-declared` | The Version does not declare a Resource of this name (whether or not one is registered) | `resource` |
| `not-registered` | Declared, but no such Resource is registered | `resource` |
| `declared-lower` | Declared at a level below what the operation needs | `resource`, `declared`, `needed` |
| `no-grant` | No Group of the User holds a Grant at the level needed | `resource`, `user`, `needed`, `held` (the highest level the User's Groups hold, or null), `contact` |
| `log-unavailable` | The Operation log entry could not be written | `resource` |

- `fix` is a sentence a Coding agent can act on. For `no-grant` it names the Resource's contact.
- A User holds the highest level any of their Groups is granted. `structure` < `read` < `write`.
- A Grant written or changed applies from the next request.
- The program decides from what it holds in memory (Applications, Versions, Resources, Grants), which it keeps exactly in step with the platform database it alone writes. So while the platform database cannot be reached, Applications are still routed and operations are still decided; what cannot be logged is refused.
- A refused or malformed operation sends nothing to the shared database.

## The Operation log (same ticket)

- Every `query` operation, allowed or refused, has one entry, written to the platform database **before** the statement is sent to the shared database and completed when it ends. If the entry cannot be written the operation is refused with `log-unavailable` and the statement is not sent.
- `GET /v1/operation-log`, by a Platform administrator: `{ "entries": [ ... ] }`, oldest first. `?limit=<n>` gives the newest `n` (1 to 1000, default 100), still oldest first; any other value is `400 malformed`.

```json
{ "id": 17, "time": "<RFC 3339>", "request": "<request id or null>", "user": "<user id or null>",
  "application": "incident-board", "version": 2, "resource": "devops", "operation": "query",
  "statement": "select id, title from incidents where open = $1",
  "outcome": "allowed" | "refused" | "failed" | "unfinished",
  "reason": "<refusal reason, when refused>", "error": "<SQLSTATE, or the limit, when failed>",
  "rows": 2, "duration_ms": 3 }
```

- `statement` is the statement **without its values**: every constant written into the SQL text is replaced by a placeholder, and parameters are never recorded. A statement that cannot be parsed is recorded as `"<unparsed>"`. No field of any entry holds a value from a statement, a parameter, or a row.
- `reason` and `error` are null when they do not apply. For a `failed` entry `error` is the SQLSTATE, the limit's name, or `unavailable`. `rows` is null unless the statement ran to its end; `duration_ms` is null unless the statement was sent. `outcome` is `unfinished` while it runs, and stays so if the program died meanwhile.
- A malformed operation has no entry. `whoami` is not an operation on a Resource and has none.
- An entry for a refusal made with no request in flight is written beside the answer, not before it, and may be dropped when the program is saturated; the program's log then has an `operations-busy` record.
- The program's own log still gets its `host-call` record for each call, as before. It records the Authorization decision at the moment of the call: a malformed call is `refused` with reason `malformed`; a call later refused with `log-unavailable` was recorded as `allowed`.

## Lower seam: the Authorization decision (same ticket)

The one place tests go below the program's outside in this ticket (spec, Testing Decisions). A crate `authz` under `crates/`, depending on nothing, with exactly this interface; its tests live in `crates/authz/tests/` and are written by the test agent.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessLevel { Structure, Read, Write }

#[derive(Clone, Copy, Debug)]
pub struct Grant<'a> { pub group: &'a str, pub level: AccessLevel }

#[derive(Clone, Copy, Debug)]
pub struct Question<'a> {
    /// Is there a request in flight?
    pub request_in_flight: bool,
    /// The Groups of the User whose request it is.
    pub groups: &'a [&'a str],
    /// What the serving Version declares for the Resource asked for, if anything.
    pub declared: Option<AccessLevel>,
    /// Is a Resource of that name registered?
    pub registered: bool,
    /// The Grants on that Resource.
    pub grants: &'a [Grant<'a>],
    /// The level the operation needs.
    pub needed: AccessLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision { Allow, Refuse(Reason) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NoRequest,
    NotDeclared,
    NotRegistered,
    DeclaredLower { declared: AccessLevel, needed: AccessLevel },
    NoGrant { held: Option<AccessLevel>, needed: AccessLevel },
}

pub fn decide(question: &Question<'_>) -> Decision;
```

The order of checks is the order of the table in "Who is allowed". It knows nothing of HTTP, Postgres or V8.

## Writing to a shared database, and the role as the boundary (ticket: Write access, and the database role as the boundary)

```js
env.VM.invoke("execute", { resource: "devops", sql: "update incidents set open = false where id = $1 returning id", params: [7] })
```

- `execute` needs Access level `write`: the Version must declare the Resource at `write` (else `declared-lower`) and a Group of the User must hold a Grant at `write` (else `no-grant`, with `held` and `needed`). Arguments and malformed arguments are as for `query`.
- It runs in one read-write transaction as the Resource's **write** login, committed when the statement succeeds and rolled back when it fails or a limit ends it.
- Answer: `{ ok: { columns, rows, row_count, rows_affected } }`. `rows` are what `RETURNING` gave (empty without it); `rows_affected` is Postgres's count for the statement.
- On a Resource registered without a write login: `{ error: { kind: "read-only-resource" } }`; nothing is sent. Its Operation log entry is `failed` with error `read-only-resource`.
- A `SELECT` through `execute` is allowed: it runs as the write login, and `rows` are its rows.
- Order for any operation: who is allowed (a refusal) first, then `read-only-resource`, then the statement check, then the database.
- The row and byte limits count the rows `RETURNING` gives.
- SQL that holds no statement is `kind-not-allowed`; a statement longer than 1 MiB is `unparsed`.
- `query` is unchanged: `read`, the read login, a read-only transaction, whatever the User's and the Version's levels are.
- In the Operation log, `operation` is `execute`, and `rows` is the rows affected. A failed `execute` records the SQLSTATE; a statement that never reached the database records why.

### The login is the boundary

Each Access level has its own connections, logged in directly as that level's login. The program never issues `SET ROLE` or `SET SESSION AUTHORIZATION`, and connections are never shared between levels or Resources. Whatever Application SQL does:

- `current_user` and `session_user` are the level's login in every operation, and no statement of an operation at `read` changes a row, a table or a sequence in the shared database, also when the read login itself would be allowed to write.
- Nothing one operation leaves in its session reaches a later operation: settings, the search path, temporary tables, prepared statements, cursors, session advisory locks, `LISTEN`. A connection that cannot be shown clean is closed, not reused.
- The limits of "Operations on a shared database" hold whatever settings Application SQL changes.

These must hold in the database itself. Tests assert them on the shared database, not on the statement check below.

### The identity label

The host's first statement in each transaction on a shared database sets three transaction-local settings, readable by Application SQL and by the database owner's own triggers: `vm.user` (the User's id), `vm.application`, `vm.request`. They are a label: Application SQL can overwrite them inside its own transaction, and nothing on the platform relies on them. They are gone when the transaction ends.

### The statement check

Before a statement is sent, the host parses it with Postgres's own parser and refuses what the platform never needs. This is hygiene, not the boundary: SQL exists that passes it and still does what it tries to stop (`docs/research/what-postgres-sees.md`).

- Allowed: exactly one statement that is a `SELECT`, `INSERT`, `UPDATE` or `DELETE`, with or without `WITH` and `RETURNING`.
- Refused: several statements; a statement that cannot be parsed; every other kind (`MERGE`, `CREATE`/`ALTER`/`DROP`, `TRUNCATE`, `SET`/`RESET`, `DO`, `CALL`, `COPY`, `EXPLAIN`, `PREPARE`/`EXECUTE`/`DEALLOCATE`, `LISTEN`/`NOTIFY`, `LOCK`, transaction control, `SELECT ... INTO`, and the rest); a call, anywhere in the statement, of `set_config`, `pg_terminate_backend`, `pg_cancel_backend`, `pg_notify`, `pg_reload_conf`, `query_to_xml` and its family, `dblink` and its family, the large-object functions (`lo_*`), the server file functions (`pg_read_file`, `pg_read_binary_file`, `pg_ls_dir`, `pg_stat_file`); a reference to `pg_authid`, `pg_shadow` or `pg_stat_activity`. A family is every function whose name starts with `query_to_xml`, `dblink` or `lo_`. Names are matched as Postgres resolves them: in any letter case, quoted, or qualified with `pg_catalog`. A listed name that is only inside a string, a comment or an alias is not a call or a reference.
- Answer: `{ error: { kind: "statement", reason: "several-statements" | "unparsed" | "kind-not-allowed" | "function-not-allowed" | "relation-not-allowed", message } }`. Nothing is sent to the shared database.
- The Operation log entry has outcome `failed` and `error` `statement`; its `statement` is recorded without values as always (`<unparsed>` when it could not be parsed).
- This replaces what the first ticket did with a statement that cannot be parsed: it is no longer sent for Postgres to refuse.

## The Application's own database (ticket: An Application's own database)

### Migrations travel with the Version

`POST /v1/applications/<name>/versions` takes one more field, optional:

```json
"migrations": [ { "name": "0000_first_tables", "sql": "<the file drizzle-kit generated, as text>" }, ... ]
```

- The list is the Application's whole history of migrations, in order, as `drizzle-kit` generates them (statements separated by its `--> statement-breakpoint` line). A name is 1 to 200 characters of letters, digits, `_` and `-`, unique in the list. Otherwise `400 malformed`, which is checked before `migrations-diverged`. A migration's `sha256` is of its text's UTF-8 bytes.
- A Version's list must begin with exactly the previous Version's list (same names, same text) and may add to it. Otherwise `409 migrations-diverged`, and nothing changes. Leaving the field out means an empty list.
- The Version's record gains `"migrations": [ { "name", "sha256" } ]`.

### What happens when a Version is sent

1. With an Application's first Version, the platform creates the Application's own database on the platform's Postgres instance, whether or not there are migrations.
2. The migrations not yet applied are applied, in order, in **one transaction**, by the platform; the platform runs no `drizzle-kit` and no Node.
3. Only then is the Version recorded and made the serving one, and the `201` answered.

If a migration fails: the transaction is rolled back, so the own database is exactly as before; no Version is recorded and no number is used up; the previous Version goes on serving; the answer is

```json
{ "error": { "reason": "migration-failed", "message": "...", "migration": "<name>",
             "code": "<SQLSTATE>", "database_message": "<Postgres's message>" } }
```

with status `422`.

### What it looks like in Postgres

For an Application the platform numbers `n`, with `<p>` the platform database's name:

- database `<p>_app_<n>`, whose comment is `vm application <name>`;
- role `<p>_app_<n>_owner`: owns every object in it and applies migrations;
- role `<p>_app_<n>_runtime`: what Application code runs as. It owns nothing. It may read and change rows in the tables the migrations made (`SELECT`, `INSERT`, `UPDATE`, `DELETE`, and use of their sequences), in tables made by later migrations too, and nothing else: it cannot create, alter or drop anything, not even temporary tables; it cannot `TRUNCATE`; it is a member of no other role; it cannot connect to the platform database or to any other Application's own database, and no other Application's roles can connect to this one.
- The schema `vm` in an own database is the platform's: it may hold the platform's own tables and functions, which the runtime role cannot change. Everything the Application's migrations make is its own, by default in `public`.
- Their passwords are the platform's own and never appear in an answer, a log record or an error message.
- The platform needs only what `VM_DATABASE_URL`'s role was promised: creating databases and roles. It does not need a superuser.
- What this does not close, because a role without superuser cannot close it or because it is the Application's own affair: every role of a Postgres instance may connect to `postgres` and `template1` unless the instance's owner revokes that; a role may change its own settings (`ALTER ROLE ... SET`) and create large objects; and a migration is the Contributors' own code, so one that creates a function running with its owner's rights gives the Application whatever that function does. The platform does not defend an Application's own database against its own Contributors.
- If an Application's first Version fails in a migration, the own database may already exist, empty. A sequence a failed migration advanced stays advanced, as in any Postgres transaction.

### Operations on the own database

`query` and `execute` take a target and an actor:

```js
env.VM.invoke("query",   { own: true, as: "user",        sql, params })
env.VM.invoke("execute", { own: true, as: "application", sql, params })
env.VM.invoke("query",   { resource: "devops", sql, params })          // as before; `as` defaults to "user"
```

- Exactly one of `own: true` and `resource` names the target; both or neither is `malformed`, and so is an `own` that is not exactly `true`. `as` is `"user"` (the default) or `"application"`; anything else is `malformed`.
- On the own database no Declared access and no Grant is needed: being served the Application is enough. Both actors are allowed whenever a request is in flight.
- **Acting as the Application is only for the own database.** `as: "application"` with a `resource` is refused with `not-own-database` (`refusal.resource` names it), whatever is declared or granted: Grants are held by Users. It is checked right after `no-request`, before anything about the Resource.
- Everything else is as on a shared database: `query` runs in a read-only transaction and `execute` in a read-write one, as the runtime role; one statement; the statement check; the limits; the answers and errors; a connection returned only clean.
- The identity label has a fourth setting, `vm.actor`: `user` or `application`. `vm.user` is the User in flight for both actors. It stays a label here too; what cannot be forged is `vm.identity()` ("Identity and Change history in the own database").
- Before an Application's first Version there is no own database; there is also no Version to serve, so no operation can reach that state.

### In the Operation log

Every entry gains `"actor": "user" | "application"` and `"own_database": true | false`. For the own database `resource` is null. `user` is the User in flight for both actors.

### The Authorization decision

The interface of `authz` gains the actor and the own database:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Actor { User, Application }

pub struct Question<'a> {
    pub request_in_flight: bool,
    /// Who the operation acts as.
    pub actor: Actor,
    /// Is the target the Application's own database? When true, `declared`,
    /// `registered`, `grants` and `groups` do not matter.
    pub own_database: bool,
    pub groups: &'a [&'a str],
    pub declared: Option<AccessLevel>,
    pub registered: bool,
    pub grants: &'a [Grant<'a>],
    pub needed: AccessLevel,
}

pub enum Reason { NoRequest, NotOwnDatabase, NotDeclared, NotRegistered, DeclaredLower { .. }, NoGrant { .. } }
```

Order: `NoRequest`; then the own database is allowed; then `NotOwnDatabase` for `Actor::Application` on a Resource; then the rest as before.

## Identity and Change history in the own database (ticket: Identity in the own database, and Change history)

The platform keeps its own things in the schema `vm` of every own database, from the moment the database is made. Three of them are part of this contract: the function `vm.identity()`, the table `vm.history`, and the setting `vm.ctx`. The rest of the schema is the platform's own affair and may change.

### Identity

For every operation on the own database the host attaches who the operation is for, before the Application's statement runs, in a way Application SQL can read and cannot make.

- `SELECT * FROM vm.identity()` gives one row: `user_id` (the User in flight), `application`, `actor` (`user` or `application`), `request`. All are text. It works in `query` and in `execute`, for both actors.
- When no identity is attached, or what is attached does not verify, `vm.identity()` raises an error with SQLSTATE `VM001`. It never answers with an empty or made-up identity.
- The attached identity lives in the transaction-local setting `vm.ctx`, as opaque text. Application SQL may read it. It is bound to the transaction it was attached in: the same text set in any other transaction, on the same connection or another, does not verify.
- Only the host can attach. Whatever the runtime role sends (setting `vm.ctx` or any other setting, calling any function in `vm` with any arguments, anything else), `vm.identity()` afterwards gives the identity the host attached to that transaction, or raises `VM001`.
- What the platform keeps secret for this is in tables of schema `vm` that the runtime role cannot read, and nowhere else in the own database.
- The label of "Operations on the own database" (`vm.user`, `vm.application`, `vm.request`, `vm.actor`) is still set, and is still only a label.

### Change history

Every table an Application's migrations make, in any schema but `vm`, is captured from the end of the migration that made it, with nothing done by the Contributor. Each row inserted, updated or deleted leaves one row in `vm.history`, written in the same transaction as the change:

| Column | Type | What |
|---|---|---|
| `id` | `bigint` | Increasing in the order the changes were recorded. |
| `at` | `timestamptz` | When the change was made. |
| `schema_name`, `table_name` | `text` | The table. |
| `kind` | `text` | `insert`, `update` or `delete`. |
| `key` | `jsonb` | The row's primary key as `{ "<column>": value, ... }`, as it was before the change (for an insert: of the new row). Null for a table without a primary key. |
| `old` | `jsonb` | The whole row before, as `to_jsonb` gives it. Null for an insert. |
| `new` | `jsonb` | The whole row after. Null for a delete. |
| `changed` | `jsonb` | For an update: the names of the columns whose value differs, as an array, in the table's column order; empty when none does. Null for an insert and a delete. |
| `user_id` | `text` | The User in flight, for both actors. |
| `application` | `text` | The Application. |
| `actor` | `text` | `user`, `application` or `migration`. |
| `request` | `text` | The request's id, as in the Operation log. Null for `migration`. |
| `migration` | `text` | For `migration`: the migration's name. Null otherwise. |

- A statement that changes many rows leaves one history row per changed row. A row changed because of another (a cascading delete, an `ON CONFLICT DO UPDATE`) is recorded like any other, under the same identity.
- A change that is rolled back leaves no history row; a history row is never left for a change that did not happen.
- Rows a migration changes in a table that an earlier migration made are recorded with `actor` `migration`, `user_id` the Contributor who sent the Version, the migration's name, and no request. Rows a migration puts into a table it makes itself are not recorded: that table's history begins when the migration ends.
- The runtime role may read `vm.history` (`SELECT`), so an Application can show its own history through `query`, under the usual limits. Whoever can use the Application can therefore read its history, as ADR 0005 says.

### A change with no identity fails

A row of a captured table cannot be inserted, updated or deleted, by any role, in a transaction with no verifiable identity: the statement fails with SQLSTATE `VM001`, nothing is changed and nothing is recorded. (A superuser, and the role that owns the tables, can still switch the capture itself off; that is not Application code, see "What this does not close".) There is no history row "made by nobody". Through the host this cannot happen to an Application; if it did, the answer would be the usual `{ error: { kind: "database", code: "VM001", ... } }`.

### What the runtime role cannot do

These hold in the database itself, for anyone logged in as, or become, the runtime role, with or without the statement check. Tests assert them there.

- Make `vm.identity()` answer anything but what the host attached: not by forging `vm.ctx`, not by replaying a `vm.ctx` read in another transaction, not by calling whatever the host calls to attach.
- Read what the platform keeps secret in `vm`.
- Stop the capture: disable, drop or replace a capture trigger or any function in `vm`; change `session_replication_role`; `TRUNCATE` a captured table or `vm.history`.
- Insert, update or delete a row of `vm.history`, other than by changing a captured row.
- Create anything in `vm`, or anything anywhere that is found before `vm`'s or the catalog's own names.

It can read: `vm.identity()`, `vm.ctx`, `vm.history`.

### The server log

With statement logging fully on in the own database (`log_statement = all`, `log_min_duration_statement = 0`, parameters logged whole), the Postgres server log gives its reader nothing to attach an identity with:

- no value the platform keeps secret in `vm` appears in it, whole or as the hex or base64 of it;
- sending again what the log shows the host sent, as the runtime role, in another transaction, attaches nothing: `vm.identity()` raises `VM001` or the statement itself fails.

### What this does not close

A migration is the Contributors' own code and runs as the role that owns the database (ADR 0013), `vm` included. A migration can therefore read the platform's secret, change `vm.history` and remove the capture. The platform records every migration's text (`vm.migrations`), so this is visible afterwards, not prevented.

## The SDK (ticket: The SDK: a typed Manifest and Trusted context, queried with Drizzle)

From this ticket on, an Application is a TypeScript project written with the SDK and Drizzle (ADR 0012, ADR 0014), built into the one classic script of "What an Application is handed". New fixture Applications are written this way; the raw host interface stays what the SDK itself is built on.

### Where things are, and what the gate needs

- The SDK is the npm package `@vm/sdk`, in `sdk/`. It has `drizzle-orm` as a peer dependency (0.45 or later, below 1.0).
- The repository root is an npm workspace (`package.json`, `package-lock.json`): `sdk`, the type tests in `blackbox/sdk-types`, and the fixture projects under `blackbox/fixtures/sdk/`. One `node_modules` at the root, so there is one copy of `drizzle-orm`.
- The gate needs Node 22 or later and `npm`. It runs `npm ci` when the lock file is newer than `node_modules`, then `npm test` at the root (the type tests, and the SDK's own checks), before `cargo test`. The black-box tests build their fixture projects with the SDK's tooling (below) and send the result to the real program.

### A project

Until the ticket on the project template, a project is these files:

```
manifest.ts                    export default defineManifest({ ... })
src/index.ts                   export default defineApp(manifest, { fetch(request, ctx) { ... } })
src/db/schema.ts               the own database's structure, as a Drizzle schema (optional)
resources/<name>/schema.ts     a shared database's structure, as a Drizzle schema (pulled; hand-written until pull exists)
resources/<name>/relations.ts  its Drizzle relations (optional)
resources/<name>/index.ts      the generated link, below
tsconfig.json                  with "paths": { "@vm/own-structure": ["./src/db/schema.ts"] }
```

The generated link from a Resource's name to its structure is exactly this file, with the name filled in (and without the `relations` lines when there is no `relations.ts`):

```ts
// Generated. Do not edit.
import type * as schema from "./schema.ts";
import type * as relations from "./relations.ts";

declare module "@vm/sdk" {
  interface SharedStructures {
    "devops": typeof schema & typeof relations;
  }
}
```

### The Manifest

```ts
import { defineManifest } from "@vm/sdk";
export default defineManifest({
  name: "incident-board",
  description: "Open incidents, for the people on call.",
  resources: { devops: { kind: "postgres", access: "read" } },
});
```

- `name`: as the API takes an Application's name. `description`: a string. `resources`: each `{ kind: "postgres", access: "read" | "write" }`; `structure` is not something an Application declares.
- The argument of `defineManifest` must be a **plain literal**: object literals with plain or quoted keys, string literals, numbers, booleans, `null`, arrays, and `as const`. No identifier, call, spread, computed key, shorthand property or template with a substitution. The file may hold other code; it is never run.

### The tooling

`@vm/sdk` ships a Node program for the build side. It is what the tool's deploy will call; nothing of it is in an Application's bundle.

- `node <sdk>/tooling/main.js manifest <project directory>` reads `manifest.ts` **without running it or anything it imports**, and prints on standard output, exit code 0:

  ```json
  { "name": "incident-board", "description": "...", "declared_access": { "resources": { "devops": { "kind": "postgres", "access": "read" } } } }
  ```

  `declared_access` is exactly what `POST /v1/applications/<name>/versions` takes.
- A Manifest that cannot be taken: exit code 2, and on standard output `{ "error": { "reason", "message", "file", "line", "column" } }`. Reasons: `manifest-missing` (no `manifest.ts`, or no `defineManifest(...)` call in it, or more than one); `manifest-not-literal` (the message says what was found and where, and that the Manifest must be a plain literal because the platform reads it without running code); `manifest-invalid` (a plain literal of the wrong shape: the message names the field). `line` and `column` start at 1 and point at the offending expression; they are `null` when there is nothing to point at (no file, no call). `file` is always the path of `manifest.ts`, as it was looked for.
- `node <sdk>/tooling/main.js bundle <project directory>` prints the Application's bundle on standard output, exit code 0: one classic script with no `import` and no `export`, which assigns `globalThis.default`, containing the Application, the SDK's runtime, Drizzle and the structures. A project that does not build: exit code 2 and `{ "error": { "reason": "build-failed", "message", "errors": [ { "file", "line", "message" } ] } }`. `bundle` does not type-check; that is `tsc`'s job.
- Any other failure: exit code 1 and a message on standard error.

### What Application code is given

```ts
import { defineApp, Refused } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const user = ctx.user();                    // { id, groups, db(name), ownDb() }
    const open = await user.db("devops").select().from(incidents).where(eq(incidents.open, true));
    await user.ownDb().insert(notes).values({ incident: 1, text: "seen" });   // as the User
    await ctx.app().ownDb().update(counters).set({ n: sql`${counters.n} + 1` }); // as the Application
    return Response.json(open);
  },
});
```

- `ctx.user()`: the User of the request in flight. `id: string`, `groups: readonly string[]`. (The sign-in ticket adds the email and the display name.)
- `ctx.user().db(name)`: a shared database declared in the Manifest, as the User. `ctx.user().ownDb()`: the own database, as the User. `ctx.app().ownDb()`: the own database, as the Application. In the types `ctx.app()` has nothing else. At run time `ctx.app().db(name)` exists all the same, so that code forced past the types gets the host's refusal `not-own-database` and its fix, not a `TypeError`.
- A handle is a Drizzle database (`drizzle-orm/pg-proxy`), typed with that database's structure, relational queries (`db.query.<table>`) included.
- **Every statement Drizzle sends is exactly one host operation**: `query` when the statement is a `select` (also one that begins with `with` and whose main statement is a `select`, and a set operation of selects such as `union`), `execute` otherwise; with `resource: <name>` or `own: true`, and `as: "user"` or `"application"`. So each shows as one Operation log entry, with the statement Drizzle built, without values.
- The handles offer no raw SQL: there is no `execute` on them. (Drizzle's `sql` fragments inside a typed query remain, as ADR 0012 says; the database role is the boundary, not the types.) `transaction` comes with its own ticket.
- A context holds no identity. Application code gets a context only in `fetch`, so it has no handle at the top level. A handle kept and used after its request is over is refused by the host with `no-request`, like any operation (or its code simply never runs, as "The raw host interface" says).

### Values

Rows come back as Drizzle's column types promise, and values written through Drizzle come back equal. At least:

| Postgres | Drizzle column | In Application code |
|---|---|---|
| `integer`, `smallint`, `serial` | `integer()`, `smallint()`, `serial()` | number |
| `bigint` | `bigint({ mode: "bigint" })` | `BigInt`, exact beyond 2^53 |
| `bigint` | `bigint({ mode: "number" })` | number |
| `numeric` | `numeric()` | string, every digit kept |
| `real`, `double precision` | `real()`, `doublePrecision()` | number |
| `boolean` | `boolean()` | boolean |
| `text`, `varchar`, `uuid` | `text()`, `varchar()`, `uuid()` | string |
| `timestamp`, `timestamptz` | `timestamp()`, `timestamp({ withTimezone: true })` | `Date`, the same instant to the millisecond, whatever the database's `TimeZone` |
| the same | `{ mode: "string" }` | the text Postgres prints |
| `date` | `date()` | `"YYYY-MM-DD"` |
| `json`, `jsonb` | `json()`, `jsonb()` | the value: object, array, string, number, boolean or null, each as itself (a JSON string `"123"` stays the string `"123"`) |
| `text[]`, `integer[]` and other arrays | `.array()` | array; elements with commas, quotes, backslashes, braces, empty strings and nulls intact |
| any, null | | `null` |

The same holds for parameters: in `values(...)`, `set(...)` and `where(...)`, and for what `returning()` gives.

For this the raw interface gained one optional argument; its answers without it are unchanged:

- `query` and `execute` take `values: "text"`. With it, every non-null value in `rows` is the text Postgres printed, as a string, and the answer gains `"types": [<the type OID of each column>, ...]`, numbers in the order of `columns`. The SDK asks for this on every operation and reads the values as Drizzle's columns expect them.
- Any other `values` (another string, not a string, `null`) is `{ error: { kind: "malformed", ... } }`.

### Refusals and failures

- An operation the host refuses rejects with `Refused`: `error instanceof Refused` is true in Application code, also for an operation made through Drizzle (the SDK takes Drizzle's own error wrapper off). `error.refusal` is the refusal as the host gave it (`reason` and its details), `error.fix` the sentence that says what to do, `error.name` is `"Refused"`, and `error.message` is `"<reason>: <fix>"`.
- Application code cannot make a `Refused` or an `OperationFailed`: `new Refused(...)` is a compile error, and at run time it throws a `TypeError`.
- A `Refused` that escapes `fetch` becomes the response `403` with `content-type: application/json` and the body `{ "refusal": { ... }, "fix": "..." }`.
- An operation that fails without being refused rejects with `OperationFailed` (also an `Error`, also `instanceof`-checkable through Drizzle): `error.kind` is the host's `error.kind` (`database`, `statement`, `limit`, `unavailable`, ...), `error.code` the SQLSTATE when `kind` is `database`, `error.detail` the host's whole error object. Escaping `fetch`, it is an uncaught error like any other: the program's fixed `500`.

### The types

The types are guidance for the Coding agent, not the boundary (ADR 0014). These are compile errors, and the tests assert the compiler's words, because a Coding agent reads them:

- `ctx.user().db("payroll")` when the Manifest does not declare `payroll`: the message lists the declared names (`... is not assignable to parameter of type '"devops" | "hr"'`).
- `insert`, `update` or `delete` through a handle declared at `read`: the message contains `'devops' is declared at 'read' in manifest.ts. Declare it at 'write' to change rows.`
- `ctx.user().db("devops")` when `devops` is declared and its structure has no generated link: using the handle fails with a message that contains `The structure of 'devops' has not been pulled.`
- `ctx.app().db(...)`: `Property 'db' does not exist on type 'ActingAsApplication'.`
- `new Refused(...)`: the constructor is private.
- `execute` on any handle: it does not exist.
- A context cannot be made by Application code from an object literal.

Forced past the types (a cast), each of these still ends at the host: `not-declared`, `declared-lower`, `not-own-database`.

## Transactions (ticket: Transactions through the SDK)

### Through the SDK

```ts
const moved = await ctx.user().ownDb().transaction(async (tx) => {
  const [grace] = await tx.select().from(users).where(eq(users.email, "grace@example.com"));
  await tx.update(tasks).set({ owner: grace.id }).where(eq(tasks.title, "write the spec"));
  await tx.insert(notes).values({ text: "reassigned" });
  return grace.id;
}, { isolationLevel: "serializable" });   // the second argument is optional
```

- Every handle has `transaction`: a shared database, the own database as the User, the own database as the Application. `tx` is a Drizzle database on the same target, with the same types (through a handle declared at `read`, `tx` offers reads only).
- When the callback returns, the transaction is committed and `transaction` resolves to what the callback returned. When it throws or rejects, everything is rolled back and `transaction` rejects with that same error. `tx.rollback()` is Drizzle's: it rolls back and `transaction` rejects with Drizzle's `TransactionRollbackError`.
- The second argument is Drizzle's: `isolationLevel` (`read committed`, `repeatable read`, `serializable`) and `accessMode` (`read write`, `read only`). Through a handle declared at `read` the transaction is always read-only.
- **Nesting is refused.** `tx.transaction(...)` rejects with an `Error` whose message contains `Transactions cannot be nested`, and nothing is sent to the host. It is not in the types of `tx`.
- A commit that cannot be made (a statement in the transaction had failed and the Application went on) rejects with `OperationFailed` of kind `transaction-aborted`; everything was rolled back.
- Statements go one at a time on the transaction's connection, in the order Application code awaited them.

### Through the raw interface

Three operations and one argument. A transaction belongs to the request that began it.

- `env.VM.invoke("tx.begin", { resource: "devops" | own: true, as, access: "read" | "write", isolation })` answers `{ ok: { tx: "<id>" } }`. `access` is required. `isolation` is optional: `"read committed"`, `"repeatable read"` or `"serializable"`; anything else is `malformed`.
  - It is decided like an operation that needs `access` on that target: the same refusals in the same order (`no-request`, `not-own-database`, `not-declared`, `not-registered`, `declared-lower`, `no-grant`). On the own database both are allowed.
  - `read` begins a read-only transaction: on a shared database as the read login, on the own database as the runtime role. `write` begins a read-write one: the write login (a Resource registered without one answers `read-only-resource` as `execute` does), or the runtime role.
  - It takes one connection from that pool and keeps it until the transaction ends. A pool with none to give within the usual wait answers `unavailable`.
- `query` and `execute` take `tx: "<id>"` **instead of** `resource`, `own` and `as` (any of those beside `tx` is `malformed`): the statement runs in that transaction, as its target and actor. Each is still one operation: decided again (a Grant taken away meanwhile refuses it), checked by the statement check, limited, and logged. `execute` in a transaction begun with `access: "read"` is refused as it would be outside (`declared-lower`, `no-grant`), or fails in the database, which refuses the write.
- `env.VM.invoke("tx.commit", { tx })` and `env.VM.invoke("tx.rollback", { tx })` answer `{ ok: {} }` and end the transaction. Committing a transaction in which a statement had failed rolls it back and answers `{ error: { kind: "transaction-aborted", message } }`.
- An id that is not an open transaction **of the request in flight** answers `{ error: { kind: "no-transaction", message } }`, for every one of `query`, `execute`, `tx.commit` and `tx.rollback`: never begun, already ended, ended by the host, or begun by another request. The answer does not say which. An id is opaque and cannot be guessed.
- A statement that Postgres refuses leaves the transaction open and aborted, as Postgres does: later statements fail with SQLSTATE `25P02` until it is rolled back.

### What the host does on its own

- **The request ends** (its response is sent, it fails, or its time is up): every transaction it left open is rolled back. Nothing it did is kept.
- **The cap.** A transaction may be open for 10 seconds. Past that the host ends it: the statement running is cancelled, everything is rolled back, and every later use of the id answers `no-transaction`. A statement in a transaction is also cut at the transaction's remaining time (`{ error: { kind: "limit", limit: "time" } }`).
- **A statement the host ends** for a limit (rows, bytes, time) ends its transaction too: rolled back, then `no-transaction`.
- **At most 4 transactions open at once per request.** A fifth `tx.begin` answers `{ error: { kind: "limit", limit: "transactions" } }`.
- A connection goes back to its pool only after its transaction is over and it is clean, as for any operation; otherwise it is closed. Two requests never share a transaction's connection: between a transaction's begin and its end, its connection runs nothing but that transaction's statements.

### Identity, the Operation log and the Change history

- The label, and on the own database the attached identity, are set once when the transaction begins and hold for all of it: `vm.identity()` answers the same in every statement, and every change is in the Change history with the User, the Application, the actor and the request, as outside a transaction.
- Every Operation log entry gains `"transaction": "<id>" | null`. Each statement in a transaction has its own entry, as before. `tx.begin`, `tx.commit` and `tx.rollback` have entries too, with `operation` `begin`, `commit` and `rollback`, `statement` the same word, and `rows` null; a refused `tx.begin` is logged like any refusal, with `transaction` null. A rollback the host makes on its own has a `rollback` entry whose `error` says why: `request-ended`, `time` or the limit's name. A commit of an aborted transaction is `failed` with `error` `transaction-aborted`.
- The id in the Operation log is the id Application code holds. Knowing an id gives nothing: it works only inside the request that began it.
