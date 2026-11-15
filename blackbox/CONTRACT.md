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
| 422 | A Version's migration breaks a rule of the platform's | `migration-refused` |
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
- A Grant written or changed applies to every operation decided after it: from the next request at the latest, and also to the later operations of a request already in flight.
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
- role `<p>_app_<n>_owner`: applies migrations and owns what they make (and, from the ticket "The Change history is kept out of a Contributor's reach" on, nothing of the platform's: see that section for the third role);
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

(Since the ticket "The Change history lives in the platform database, shipped from an outbox" these rows are kept in the platform database's `change_history`, not in `vm.history`, and are read through the operation `history`: see "The Change history in the platform database". What a change records is as described here.)

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

A row of a captured table cannot be inserted, updated or deleted, by any role, in a transaction with no verifiable identity: the statement fails with SQLSTATE `VM001`, nothing is changed and nothing is recorded. (A superuser, and the role that owns the tables, can still switch the capture itself off; for what Application code can make of the owner's rights, see "What a migration leaves behind".) There is no history row "made by nobody". Through the host this cannot happen to an Application; if it did, the answer would be the usual `{ error: { kind: "database", code: "VM001", ... } }`.

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

(Closed by the ticket "The Change history is kept out of a Contributor's reach"; what follows is how it stood before.)

A migration is the Contributors' own code and runs as the role that owns the database (ADR 0013), `vm` included. A migration can therefore read the platform's secret, change `vm.history` and remove the capture. The platform records every migration's text (`vm.migrations`), so this is visible afterwards, not prevented.

## The SDK (ticket: The SDK: a typed Manifest and Trusted context, queried with Drizzle)

From this ticket on, an Application is a TypeScript project written with the SDK and Drizzle (ADR 0012, ADR 0014), built into the one classic script of "What an Application is handed". New fixture Applications are written this way; the raw host interface stays what the SDK itself is built on.

### Where things are, and what the gate needs

- The SDK is the npm package `@vm/sdk`, in `sdk/`. It has `drizzle-orm` as a peer dependency (0.45 or later, below 1.0).
- The repository root is an npm workspace (`package.json`, `package-lock.json`): `sdk`, the type tests in `blackbox/sdk-types`, and the fixture projects under `blackbox/fixtures/sdk/`. One `node_modules` at the root, so there is one copy of `drizzle-orm`.
- The gate needs Node 22 or later and `npm`. It runs `npm ci` when the lock file is newer than `node_modules`, then `npm test` at the root (the type tests, and the SDK's own checks), before `cargo test`. The black-box tests build their fixture projects with the SDK's tooling (below) and send the result to the real program.

### A project

A project is these files (the section "A project: the template, creating one and checking one" says what a new one holds besides):

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
- When the callback returns, the transaction is committed and `transaction` resolves to what the callback returned. When it throws or rejects, everything is rolled back and `transaction` rejects with that same error. `tx.rollback()` is Drizzle's: it rolls back and `transaction` rejects with Drizzle's `TransactionRollbackError`, as Drizzle makes it (`error instanceof TransactionRollbackError`; Drizzle 0.45 gives it the `name` `DrizzleError`).
- The second argument is Drizzle's: `isolationLevel` (`read committed`, `repeatable read`, `serializable`) and `accessMode` (`read write`, `read only`). Through a handle declared at `read` the transaction is always read-only. The SDK begins the transaction with `access: "read"` when the handle is declared at `read` or `accessMode` is `read only`, and with `access: "write"` otherwise.
- **Nesting is refused.** `tx.transaction(...)` rejects with an `Error` whose message contains `Transactions cannot be nested`, and nothing is sent to the host. It is not in the types of `tx`.
- A commit that cannot be made (a statement in the transaction had failed and the Application went on) rejects with `OperationFailed` of kind `transaction-aborted`; everything was rolled back.
- Statements go one at a time on the transaction's connection, in the order Application code awaited them.

### Through the raw interface

Three operations and one argument. A transaction belongs to the request that began it.

- `env.VM.invoke("tx.begin", { resource: "devops" | own: true, as, access: "read" | "write", isolation })` answers `{ ok: { tx: "<id>" } }`. `access` is required and is `"read"` or `"write"`; anything else, `"structure"` included, is `malformed`. `isolation` is optional: `"read committed"`, `"repeatable read"` or `"serializable"`; anything else is `malformed`. Without it the transaction has the database's default isolation.
  - It is decided like an operation that needs `access` on that target: the same refusals in the same order (`no-request`, `not-own-database`, `not-declared`, `not-registered`, `declared-lower`, `no-grant`). On the own database both are allowed.
  - `read` begins a read-only transaction: on a shared database as the read login, on the own database as the runtime role. `write` begins a read-write one: the write login (a Resource registered without one answers `read-only-resource` as `execute` does), or the runtime role.
  - It takes one connection from that pool and keeps it until the transaction ends. A pool with none to give within the usual wait answers `unavailable`.
- `query` and `execute` take `tx: "<id>"` **instead of** `resource`, `own` and `as` (any of those beside `tx` is `malformed`): the statement runs in that transaction, as its target and actor. Each is still one operation: decided again (a Grant taken away meanwhile refuses it), checked by the statement check, limited, and logged. `execute` in a transaction begun with `access: "read"` is refused as it would be outside (`declared-lower`, `no-grant`), or fails in the database, which refuses the write.
  - **In a transaction begun with `access: "write"`, every statement needs `write`, `query` included.** There a `query` runs as the write login in a read-write transaction, where Postgres would let it write (`with moved as (insert ... returning ...) select ...`); outside a transaction a `query` is held to reading by its read-only transaction. So a User whose Grant is lowered from `write` to `read` while such a transaction is open has its next statement refused `no-grant` with `needed` `write`, whether it is a `query` or an `execute`. The Operation log entry still says `query`.
- `env.VM.invoke("tx.commit", { tx })` and `env.VM.invoke("tx.rollback", { tx })` answer `{ ok: {} }` and end the transaction. Neither is decided again: a commit keeps what was allowed when it was done. A `tx` that is missing or not a string is `malformed`, here and on `query` and `execute`. Committing a transaction in which a statement had failed rolls it back and answers `{ error: { kind: "transaction-aborted", message } }`.
- An id that is not an open transaction **of the request in flight** answers `{ error: { kind: "no-transaction", message } }`, for every one of `query`, `execute`, `tx.commit` and `tx.rollback`: never begun, already ended, ended by the host, or begun by another request. The answer does not say which. An id is opaque and cannot be guessed. Such a use has no Operation log entry: nothing was touched (the program's log has its `host-call` record).
- A statement that Postgres refuses leaves the transaction open and aborted, as Postgres does: later statements fail with SQLSTATE `25P02` until it is rolled back.

### What the host does on its own

- **The request ends** (its response is sent, or it fails): every transaction it left open is rolled back. Nothing it did is kept. A statement still running in one at that moment is cancelled; its Operation log entry is `failed` with `error` `request-ended`. (The ticket on limits per request adds the request's own time limit to this.)
- **The cap.** A transaction may be open for 10 seconds. Past that the host ends it: the statement running is cancelled, everything is rolled back, and every later use of the id answers `no-transaction`. A statement in a transaction is also cut at the transaction's remaining time (`{ error: { kind: "limit", limit: "time" } }`).
- **A statement the host ends** for a limit (rows, bytes, time) ends its transaction too: rolled back, then `no-transaction`.
- **At most 4 transactions open at once per request.** A fifth `tx.begin` answers `{ error: { kind: "limit", limit: "transactions" } }`.
- A connection goes back to its pool only after its transaction is over and it is clean, as for any operation; otherwise it is closed. Two requests never share a transaction's connection: between a transaction's begin and its end, its connection runs nothing but that transaction's statements.

### Identity, the Operation log and the Change history

- The label, and on the own database the attached identity, are set once when the transaction begins and hold for all of it: `vm.identity()` answers the same in every statement, and every change is in the Change history with the User, the Application, the actor and the request, as outside a transaction.
- Every Operation log entry gains `"transaction": "<id>" | null`. Each statement in a transaction has its own entry, as before. `tx.begin`, `tx.commit` and `tx.rollback` have entries too, with `operation` `begin`, `commit` and `rollback`, `statement` the same word, and `rows` null; a refused `tx.begin` is logged like any refusal, with `transaction` null. A rollback the host makes on its own has a `rollback` entry with `outcome` `allowed` whose `error` says why: `request-ended`, `time` or the limit's name. A commit of an aborted transaction is `failed` with `error` `transaction-aborted`. A `tx.begin` that could not begin is `failed` with `transaction` null and `error` `transactions` (the fifth at once) or `unavailable` (no connection to give). A use of `tx` with no request in flight is refused `no-request` and has no entry, since no target is known.
- The id in the Operation log is the id Application code holds. Knowing an id gives nothing: it works only inside the request that began it.

## The platform's part of an own database is out of a migration's reach (ticket: The Change history is kept out of a Contributor's reach)

ADR 0017. A migration is a Contributor's code. From this ticket on it cannot read or change what the platform keeps in the own database, cannot end the platform's transaction, and cannot leave a table without its capture. This section replaces what earlier sections say of the role `_owner`, and "What this does not close" under "Identity and Change history in the own database".

### Three roles

For an Application the platform numbers `n`, with `<p>` the platform database's name:

- `<p>_app_<n>_platform`: the platform's own role for this database. It owns the database and the schema `vm` with everything in it.
- `<p>_app_<n>_owner`: what migrations run as. It owns what the migrations make and nothing else. **It cannot log in** (`rolcanlogin` false) except while the platform is applying migrations, and **no password a migration gave it is ever good**: see "The migration role's password" below.
- `<p>_app_<n>_runtime`: as before.
- The platform still needs only `CREATEDB` and `CREATEROLE`.
- An own database in the earlier form (its `vm` owned by `_owner`) is brought to this form when the program starts, before it serves; its history, its keys and its tables' rows are unchanged.

### What a migration cannot do

Each of these, in a migration's text, fails in the database: the answer is `422 migration-failed` with the SQLSTATE given, nothing the Version's migrations did is kept, and the keys, the history, the record of migrations and the capture are as before.

- Read `vm.keys` (`42501`).
- Insert, update, delete or truncate `vm.history` or the platform's record of migrations (`vm.migrations`); lock them; put a rule, a trigger or a child table on them (`42501`).
- Create, replace, alter, rename, take over or drop anything in `vm`, or `vm` itself; move a table into it (`42501`).
- Become the platform's role or the runtime role, give itself their rights or theirs to another, change the database's settings or owner (`42501`, or another refusal of Postgres's own; a few such attempts Postgres answers with a warning and does nothing, and then the Version applies and the platform's part is as before).
- A migration may change the migration role's own password or settings (Postgres lets any role); it is not refused and is of no use: the platform sets what it needs each time, and see "The migration role's password" below.
- Make a trigger of its own that calls a function of `vm` (`42501`): the capture's functions are the platform's alone to attach. So a capture trigger that a migration drops cannot be made again by it, in the right form or with old and new exchanged.
- `TRUNCATE` a table that has history, directly, through `CASCADE` or through a partition (`VM002`; the message says to `DELETE`, which is recorded).

It can still read `vm.history`, and `vm.identity()`, which during a migration answers the Contributor who sent the Version as `user_id`, `actor` `migration` and no request.

Whatever else a migration tries that Postgres refuses to a role without superuser (temporary objects, untrusted languages and extensions, event triggers, publications of all tables, the server's files, other sessions, the statistics), the Version fails or applies as Postgres decides, and either way the platform's part is as before.

### How migrations are sent, and what is refused before anything is sent

- The platform splits each migration at `drizzle-kit`'s breakpoint lines and then into single statements with its own Postgres parser, and sends them one at a time.
- A Version is refused with `422` and

  ```json
  { "error": { "reason": "migration-refused", "rule": "<kebab-case>", "message": "<what was found, where, and what to do instead>", "migration": "<name>" } }
  ```

  when a rule of the platform's is broken. When several are broken the answer names one of them. `migration` is the migration whose text broke the rule, or after which the check failed. Nothing is kept, no Version is recorded, the previous Version goes on serving, exactly as for `migration-failed`. In the order of "The platform's API", `migration-refused` stands where `migration-failed` does.
- Rules checked on the text, before any statement of any migration is sent:
  - `transaction-control`: a statement that begins, ends, chains, prepares or savepoints a transaction (`BEGIN`, `START TRANSACTION`, `COMMIT`, `END`, `ROLLBACK`, `ABORT`, `COMMIT AND CHAIN`, `ROLLBACK AND CHAIN`, `PREPARE TRANSACTION`, `COMMIT PREPARED`, `ROLLBACK PREPARED`, `SAVEPOINT`, `RELEASE`, `ROLLBACK TO`). The word inside a string, a comment, a dollar-quoted body or an identifier is not one.
  - `unparseable`: text the parser cannot read. The message gives Postgres's own words and the line of the migration.
  - `copy-stream`: `COPY ... FROM STDIN` or `COPY ... TO STDOUT`. A migration is sent statement by statement and has no stream to give or take; rows are put in with `INSERT`.
- A `DO` block or a procedure that tries to commit fails in the database (`2D000`), which is `migration-failed`.

### What is checked before a Version's migrations are committed

After the last migration's last statement, and before the commit, the platform makes sure nothing of the migration's can still run, then checks. A broken rule is `migration-refused`:

- `capture-changed`: a capture trigger that existed before one of the Version's migrations (the capture of a table is made at the end of the migration that makes the table) was disabled, dropped or renamed during a later one, **also when it was put back** (disable, change rows, enable). The message names the table. (A migration that drops one and tries to make it again fails at that statement with `42501`, which is `migration-failed`.)
- `capture-missing`: a table outside `vm` is without its whole capture at the end, or carries another trigger that calls a function of `vm`.
- `deferrable`: a trigger or a constraint that is `DEFERRABLE`. Their work would run at the commit, after the checks. (A constraint that failed when made immediate is `migration-failed` with its own SQLSTATE.)
- `json-cast`: a cast from one of the Application's types to `json` or `jsonb` whose function is not Postgres's own. Postgres would call it inside the capture, with the platform's rights.
- `in-vm`: an extension, or any object the platform did not make, in schema `vm` (`CREATE EXTENSION hstore SCHEMA vm` is allowed by Postgres to this role).
- `security-definer`, `index-code`, `type-code`: see "What a migration leaves behind" below.

A cursor left open `WITH HOLD` is closed by the platform before the checks; its query never runs, and the Version applies.

A cast of the `json-cast` kind followed by a change of a captured table in the same Version may also end as `migration-failed` with `VM003` (below): whichever comes first.

### The capture itself

- A row of a table with a column named `t`, `vm_row`, `vm_new` or `vm_old`, of any type, is recorded whole, like any row.
- What is recorded does not depend on the writer's session settings: with `extra_float_digits`, `TimeZone`, `IntervalStyle`, `DateStyle` or `bytea_output` set otherwise in the transaction that writes (a migration can set them; the runtime role can through its own role settings), the history row holds every digit, and times with the offset `+00:00`.
- While a cast of the `json-cast` kind exists, every change of a captured table fails with `VM003`, whoever makes it. (A migration that makes one is refused, so this is seen only if one was made otherwise.)
- A virtual generated column (Postgres 18) is recorded as null: it holds no data of its own, and the capture computes nothing. Postgres itself refuses one that calls a function of the Application's.

### The migration role's password

A migration can give the migration role a password (`ALTER ROLE CURRENT_USER PASSWORD '...'`), hoping to log in as the owner of the Application's tables once the migrations are committed.

- No login as `_owner` with a password a migration set ever succeeds: not while the Version is being applied, not after it is accepted or refused, **not when the platform is held up or stopped between committing the migrations and closing the role**. From the commit on, the role has no password.
- When closing the role is held up (the platform's `ALTER ROLE` waits on a lock, or fails), the Version is still answered as its migrations ended, at most 15 seconds after their commit; the role is closed before the own database is next used, and the program's log has an `own-database-migration-role-not-closed` record naming the Application.
- When the program is stopped while a Version's migrations are being applied: nothing of the Version is kept (or all of it, if the commit was made); at the next start, before the program serves, `_owner` cannot log in and has no session; the same Version can be sent again.
- When the client that sent the Version goes away before the answer: the same, without a restart. Within 15 seconds `_owner` cannot log in and has no session, and the migrations were applied whole or not at all.
- At most 8 Versions are being applied at once in the program, whoever sent them and whether or not their senders still wait. One more is answered `503 unavailable` at once and nothing of it is kept; it can be sent again.
- Two first Versions of one Application sent at once: one own database and one set of roles are made; each is answered `201` or with a refusal of "The platform's API" (`migrations-diverged` when their lists differ), never `500`; those accepted are numbered 1 and 2; `_owner` cannot log in afterwards.

### What a migration leaves behind

Code a migration leaves in the database can run later with the migration role's rights, though the role cannot log in: Postgres runs a `SECURITY DEFINER` function as its owner, and it runs the triggers, constraints and index expressions of a table as the table's owner while it carries out a foreign key's `ON DELETE` or `ON UPDATE` action, analyzes the table or refreshes a materialized view (`docs/research/change-history-out-of-reach.md`, part F). As the owner such code could switch the capture off, change rows and switch it on again, with no migration running. The platform holds run time to what it holds a migration to.

**Refused when the migration is applied** (`migration-refused`, checked before the commit with the other rules):

- `security-definer`: a function or a procedure of the migrations' marked `SECURITY DEFINER`, in any schema. The message names it and says the runtime role may already read and write every table the migrations make.
- `index-code`: an index (in an expression or in its `WHERE`) or an extended statistic that calls a function a role of the Application's owns (the migration role, or the runtime role where a migration let it create), directly or through an operator, a cast or a domain of theirs (a domain's constraint can be given a function later). Postgres's autovacuum would run it as the owner. The message names the index or statistic and the function. Indexes on Postgres's own functions and operators (`lower(name)`, `(a + b)`, `WHERE deleted_at IS NULL`) are allowed, and on a trusted extension's (`pg_trgm`, `citext`): those belong to a superuser, and no role of the Application's can change them. A function of the Application's that a migration adds to an extension it installed (`ALTER EXTENSION ... ADD FUNCTION`) is still the Application's, and refused.
- `type-code`: a range type whose `subtype_diff` is a function a role of the Application's owns, whether or not a table has a column of it yet. Postgres's autovacuum would run that function as the owner of any table with such a column, with no index on it. The message names the type and the function, and says that `subtype_diff` may be left out or be one of Postgres's own (`float8mi`, `int4range_subdiff`). A range type with no `subtype_diff`, or with one that belongs to a superuser, is allowed, and so are Postgres's own range types. The function may come from an earlier migration or an earlier Version than the type.

**Checked before every commit that could carry a change.** On the own database, an `execute` outside a transaction, and the commit of a transaction begun with `access: "write"`, end like this: the host makes deferred constraints immediate, closes every cursor (one declared `WITH HOLD` never runs its query), and checks that **the statement or the transaction changed neither the structure of a table nor the capture on one**. If it did:

- everything the statement or the transaction did is rolled back, what it did to the structure and the capture included;
- the answer is `{ error: { kind: "database", code: "VM004", message } }` (or, when a deferred trigger that such code queued fails while the host makes deferred constraints immediate, Postgres's own error for that: `42501` on Postgres 16 and 17, where the trigger runs without the owner's rights), from `execute` or from `tx.commit` (through the SDK: `OperationFailed` of kind `database` with that code); the message says that an Application's structure changes in migrations only;
- the Operation log entry (of the `execute`, or of the `commit`) is `failed` with `error` `VM004` (or `42501` in that case); the transaction is over, as after any commit.

At the least these are such a change, whoever's rights the statement that made them ran with, whoever comes to own what it made, and however it came to run:

- a capture trigger disabled, dropped or renamed, **also when it was put back** in the same transaction;
- a table (outside `vm`) made, dropped, renamed or moved to another schema;
- a column added, dropped, or given another type;
- a trigger or a constraint made that is `DEFERRABLE`;
- a partition attached or detached.

The platform refuses more than these: whatever takes a lock on a table, an index, a view or a sequence that reading and writing rows does not need is answered the same way. That is every change of a definition, and also `LOCK TABLE` in a mode stronger than `ROW EXCLUSIVE`, `ANALYZE`, and the refresh of a materialized view, when a function of the Application's does them. At run time an Application reads and writes rows; everything else is a migration's. What reading and writing rows brings with it is as before: a foreign key's cascade with the Application's own triggers and constraints, functions that read and write rows, `SELECT ... FOR UPDATE`, sequences, `ON CONFLICT`, any isolation level.

A `query` outside such a transaction is in a read-only transaction, where Postgres itself refuses such a change (`25006`). A lock or an `ANALYZE` that Postgres allows there changes nothing and ends with the `query`; it is let through.

This is the host's doing, on every connection it gives an Application; the runtime role's password is the host's alone.

**What something else changed.** A change made by something that is not a transaction of the host's (a way nobody foresaw; a superuser or somebody else with the database's own rights) cannot be rolled back by the platform. It is found and said:

- The platform keeps, in the platform database, what the structure and the capture of each own database were when the last Version's migrations ended, and compares: when it first uses the own database after the program starts (so also a change made while the program was not running is found); when a Version is sent, before its migrations are applied; and with a write, whenever the last comparison is 30 seconds old or older. When it finds them otherwise, it
  - writes an Operation log entry: `operation` and `statement` `capture-check`, `outcome` `failed`, `error` `changed-outside`, `application`, `own_database` true, `actor` `platform`, and null `user`, `request`, `resource`, `transaction`, `rows`;
  - writes a `capture-changed-outside` record to the program's log, with `application` and `tables`: the tables, as `"<schema>.<table>"`, whose structure or capture is not what it was (one that was dropped or made is named too);
  - puts the capture back whole on every table: a capture trigger found disabled is enabled, one missing is made, a table found without capture is given it. A table the platform may not put the capture on, because it belongs to a role that is not the Application's migration role (a superuser made it there), is left as it is and named in the record's `uncaptured` (an array like `tables`, empty otherwise); while it is there, a Version with migrations to apply is refused `capture-missing`, naming it, and a Version with none to apply is accepted;
  - and goes on from what it now finds.
- The write that finds it may itself be answered `VM004` and rolled back. The next write is not, and is recorded in the history. A Version that finds it is applied as it would have been.
- One change is said once: not again at the next write, nor after a restart. And it is never lost: the entry and what the platform keeps are written together, so when the entry cannot be written, nothing is kept and the change is found and said at the next occasion.
- A comparison that cannot be made or finished now (the own database does not answer in time, a lock is in the way of putting the capture back) leaves an `own-database-not-compared` record with `application` in the program's log, is made again at the next occasion (a start, a Version, or a write 30 seconds or more after it), and holds no operation up for more than 20 seconds; the operation then goes on as it would have. The same holds while the entry cannot be written: the operation or the Version that found the change is answered as it would have been.
- A Version's migrations and what the platform keeps of them are two databases' affairs. Whenever the program is stopped between them, or keeping fails after the commit (the program's log then has `own-database-structure-not-kept` with `application`, and the Version is still answered `201`), nothing false is said afterwards and nothing true is lost: after a stop before the commit, nothing is found changed and the Version can be sent again; after a stop or a failure after the commit, nothing is found changed either, and a change made outside later is found as always. (After a stop after the commit the Version itself may not be recorded; sent again, it is accepted, its migrations being applied already.)
- Ordinary maintenance is not a change: after `VACUUM (FREEZE, ANALYZE)`, `VACUUM FULL` (of a table, of the catalogs), `REINDEX`, `CLUSTER` and `ANALYZE` by a superuser, no such entry is written and writes go on.
- After the Postgres under the platform is replaced (a restore of a dump, `pg_upgrade`), each own database is found changed once: to the platform it is every table changed at once, the same finding as any above.
- A Version sent while writes are being made does not make them fail: a write is checked against what holds for the Version whose migrations were last committed, and none is answered `VM004` for the Version's own change.

**The platform's own sessions run nothing of the Application's.** Whatever a migration left in `public` or any schema of its own (functions, operators, types of any name), no session the platform opens as `_platform` in the own database calls it: not when the database is brought from an earlier form, not when the keys are read, not when the migration role is closed.

### What this does not close

- A superuser, and whoever else holds the database's own rights outside the platform, can still switch the capture off. The platform says so when it finds it ("What something else changed"); it cannot say which rows were changed meanwhile.
- A materialized view cannot be refreshed at run time: its refresh runs the view's query as the owner and takes a lock that reading and writing rows does not need. It is filled when a migration makes it or refreshes it.
- **A column or a table dropped, and a table replaced under its name,** leave no history row per row: each is one `structure` entry of the table's history, with the Contributor and the migration's name ("Changes of structure that change data"). A table whose rows a migration writes anew is recorded row by row ("A rewrite records each row it changed").
- The Application's own triggers, which a migration may make on its own tables, change rows under the User in flight and are recorded so. That is the Application's behaviour, as any of its code is.
- (Closed by the next ticket: the history is in the platform database.)

## Reading the Change history (ticket: Read a row's Change history through the SDK)

(How a read reaches the host, where the history is kept and what a `structure` entry is: see "The Change history in the platform database", which replaces what this section says of `vm.history` and of `query`.)

An Application reads the Change history of its own database through the SDK, to show who changed what. Nothing restores from it.

```ts
import type { Change } from "@vm/sdk";
import { tasks } from "./db/schema.ts";

const changes: Change<typeof tasks>[] = await ctx.user().ownDb().history(tasks, { id: 7 });
for (const change of changes) {
  change.kind;        // "insert" | "update" | "delete"
  change.old?.title;  // the row before, typed as `select()` types a row of `tasks`; null for an insert
  change.new?.title;  // the row after; null for a delete
  change.changed;     // for an update: the properties whose value differs; null otherwise
  change.userId;      // the User the change was made for
  change.application;
  change.actor;       // "user" | "application" | "migration"
  change.at;          // Date
}
```

### The call

- `history(table, key?, options?)` is on both handles of the own database: `ctx.user().ownDb()` and `ctx.app().ownDb()`. It resolves to an array of `Change<typeof table>`, **newest first**.
- `table` is a table of the own database's structure (a Drizzle table, in any schema; one declared without a schema is read in `public`, where its migrations make it). Anything else throws a `TypeError`.
- `key` names one row by its primary key: an object with exactly the table's primary key columns, by their property names in the Drizzle table (`{ id: 7 }`; for a composite key `{ orderId: 3, line: 2 }`). A key with a property missing, or one that is not of the primary key, or given for a table without a primary key, throws a `TypeError` that names the primary key's properties; nothing is sent to the host. A key the types can check (the wrong property, the wrong type of value, for a primary key declared on its column with `.primaryKey()`) is a compile error.
- Without `key` (or with `undefined`), the changes of every row of the table. That is the only way to read the history of a table without a primary key. `null` is not a key: it throws the same `TypeError`, so that a value that was meant to be there and is not does not read the whole table.
- The history is kept under the key the row had **before** each change. A row whose primary key was updated has its earlier changes, and that update, under the old key, and its later ones under the new.
- `options.limit`: how many changes at most; an integer from 1 to 1000, by default 100. Anything else (`null` too; `undefined` is the default) throws a `TypeError`, nothing sent. `options.before`: a change's `id`, a string of decimal digits; only changes older than it are answered. Anything else but `undefined` throws a `TypeError`, nothing sent. With both, an Application pages back through a long history.
- It is not on a shared database's handle (Milestone 1 keeps no Change history there) and not on a transaction's `tx`. Neither has it in its types. Forced past them on a shared handle, it throws an `Error` whose message contains `Change history is kept only in the own database`, and nothing is sent to the host.

### A change

| Property | Type | What |
|---|---|---|
| `id` | `string` | The change's place in the history: `vm.history.id`, as decimal digits. Later changes have greater ids (compare as numbers, or use `before`). |
| `at` | `Date` | When the change was made. |
| `kind` | `"insert" \| "update" \| "delete"` | |
| `old` | the table's row, or `null` | The whole row before; `null` for an insert. |
| `new` | the table's row, or `null` | The whole row after; `null` for a delete. |
| `changed` | `string[] \| null` | For an update, the properties whose value differs, in the order the Drizzle table declares them; `null` for an insert and a delete. |
| `userId` | `string` | The User in flight when the change was made, whichever the actor; for `migration`, the Contributor who sent the Version. |
| `application` | `string` | |
| `actor` | `"user" \| "application" \| "migration"` | |
| `request` | `string \| null` | The request's id, as in the Operation log; `null` for `migration`. |
| `migration` | `string \| null` | The migration's name, for `migration`; `null` otherwise. |

- `old` and `new` are rows exactly as `select()` from that table gives them: the same properties, and each value as "The SDK: Values" promises for its column (a `bigint({ mode: "bigint" })` exact beyond 2^53, a `timestamp` a `Date`, a `numeric` a string with every digit, `jsonb` the value, an array an array). A row read from the history equals the row a `select()` gave at that time.
- **They are read in the table's present shape.** A column a later migration dropped is not shown; a column added later is `null` in rows from before it; a renamed column is `null` under its new name in rows from before the rename. In `changed`, a column the Drizzle table does not declare (one the table no longer has) is given by its database name, after the others. A value that no longer fits its column's present type (a later migration changed the type and the old value does not cast to it) makes a read whose answer would hold it fail whole with `OperationFailed` of kind `database` (its one Operation log entry is `failed`, with the SQLSTATE as `error`): the Application cannot read that part of the table's history through the SDK until the structure fits again (a read whose `limit` and `before` keep such a change out of the answer is not touched by it). What is stored is never changed by this; a `query` on `vm.history` through the raw interface still shows it.

### An operation like any other

- Each `history(...)` call is exactly one host operation: a `query` on the own database, as the handle's actor, for the request in flight. It has one Operation log entry (`operation` `query`, `resource` null, the actor, `outcome`, `rows` the number of changes answered) whose `statement` reads from `vm.history` and holds no value: not the key, not the limit.
- It is decided and limited as any `query`: with no request in flight it is refused `no-request` (rejects with `Refused`), should such code ever run (through the SDK it does not: once the response is given, neither a timer nor the continuation of an awaited call runs, so no black-box test shows this); the limits on rows, bytes and time apply (`OperationFailed` of kind `limit`).
- Whoever can use the Application can read all of its history through it, as the Application's code decides (ADR 0005). The history says who changed a row; it does not restrict who may read that.

### Only its own, and only to read

- The history an Application reads is its own database's. There is nothing by which Application code can name another Application's own database: `own: true` is always the database of the Application in flight. Two Applications with a table of the same name and a row of the same key each read only their own changes.
- The SDK has no call that writes, changes or deletes history, and exports no table object for `vm.history`. An Application that declares `vm.history` as a Drizzle table of its own and inserts, updates or deletes through it gets `OperationFailed` of kind `database` with code `42501` (the runtime role may only read it), and the history is unchanged. So is one that does the same through the raw interface.

## The Change history in the platform database (ticket: The Change history lives in the platform database, shipped from an outbox)

ADR 0017, second slice. The Change history is no longer kept in the Application's own database. This section replaces what earlier sections say of the table `vm.history`, of reading it with `query`, and of how `history(...)` reaches the host; what they say of what a change records (its columns, one row per changed row, the identity, rolled-back changes, migrations' changes) holds as before.

### Where it is kept

- **In the own database there is no `vm.history`**: for every role, a statement that names it fails with `42P01`. The capture writes each change, in the same transaction as the change, to the outbox, the table `vm.outbox`. Its columns are the platform's own affair, and only the platform's role can read or change it (the tests, as the Postgres administrator, only count its rows: it is empty once everything committed has been shipped). For the migration role and the runtime role, reading it, writing it, locking it or truncating it fails with `42501`, as for anything else in `vm` they were not given.
- **The history is in the platform database**, in the table `change_history`, one row per Application and change:

| Column | Type | What |
|---|---|---|
| `application` | `text` | The Application's name. |
| `id` | `bigint` | The change's id: increasing, per Application, in the order the changes were recorded. |
| `at`, `schema_name`, `table_name`, `kind`, `key`, `old`, `new`, `changed`, `user_id`, `actor`, `request`, `migration` | | As "Change history" gives them. `kind` may also be `structure` (below). |
| `structure` | `jsonb` | For `structure`: what changed. Null otherwise. |

  `(application, id)` is unique. The tests read this table as the Postgres administrator; the rest of how the platform keeps it is its own affair.
- **No role of an own database can connect to the platform database**: none of the platform's role for that database, the migration role and the runtime role has `CONNECT` on it, and one that can log in is refused with `42501`. So none can read or change the history where it is kept. The program takes `CONNECT` on the platform database from `PUBLIC` when it starts. When it may not (IT made the database and owns it) and `PUBLIC` still has it, the program does not start: it exits with a status other than 0 and a `platform-database-open` record whose `statement` is what to run (`REVOKE CONNECT ON DATABASE "<name>" FROM PUBLIC`). Once `PUBLIC` has it no longer, the program starts in such a database (the tests give its role `CONNECT`, `CREATE` and `TEMPORARY` on the database and `USAGE` and `CREATE` on its schema `public`).
- **No API for Platform administrators reads the history.** `GET /v1/operation-log` shows that a history was read and of which table, never a row of it; no route under `/v1` answers changes.
- An own database from before this ticket is brought to this form when the program starts, before it serves: once the program listens, `vm.history` is gone. What it held is shipped like anything in the outbox (by the shipment at start, and at the latest before the next read of that Application's history) and is then in `change_history`, each change once and with the `id` it had; later changes have greater ids.

### Shipping

The host ships what the outbox holds to `change_history` and then deletes it from the outbox.

- **When.** After every `execute` and every committed transaction on the own database, **after it was answered** (the answer does not wait for the shipment); after a Version's migrations are committed and the Version answered; before every read of the history; and for every own database when the program starts, without delaying serving.
- **Exactly once.** Every change that was committed is in `change_history` exactly once, and a change that was rolled back is never there. This holds also when:
  - the program is stopped (killed) between a change's commit and its shipment, or in the middle of a shipment: after the next start the change is there, once;
  - a shipment fails or is held up (the tests hold a lock on `change_history`): the write that caused it was answered all the same, the changes stay in the outbox, and the next shipment takes them. The program's log has a `history-not-shipped` record with `application` and nothing of any row;
  - shipments are asked for at once: many writes and reads of the history at the same time, for one Application and for several.
- **How soon.** With both databases answering, a change is in `change_history` within 10 seconds of its write's answer (the tests wait that long), and always before a read of the history that began after the write was answered gives its answer.
- **Bounded.** Changes are shipped in batches: each batch is one transaction in the platform database, with no savepoint (its rows in `change_history` share one `xmin`), and holds at most 1,000 changes and, past its first change, at most 4 MiB of `old` and `new` as text. Each step of a batch (reading the outbox, writing `change_history`, deleting from the outbox) is ended after 10 seconds. One shipment runs per own database at a time. A statement that changes 100,000 rows is shipped whole, in such batches; the time that takes is written down by the test that does it.
- **Nobody waits for what is not theirs.** A read of the history waits only for the changes committed before it began: writes made after that, however many and however fast, do not hold it up. One Application's shipments do not hold up another's: the relay takes turns of one batch each, so while several Applications ship large outboxes, a write of another Application is shipped and a read of its history answered as soon as their own changes are. Two bounds, and what happens at each:
  - Only a read is given a place to wait for a shipment, never a write: writes, however many requests they are and whenever they are made, take no place and move no read's. One own database has 64 such places for reads begun at different moments. While they are all taken, a read that begins waits also for what was committed until a place came free; it is still answered, or ended at its `time` limit, and never answered without what was committed before it began.
  - The relay keeps at most 32 connections to own databases at once (their `application_name` is `vm relay`), and an own database keeps one no longer than one batch while another waits for one. So the promise holds also when more than 32 own databases have large outboxes to ship at once: a write of yet another Application is shipped, and a read of its history answered, after a number of batches that does not grow with the size of the others' outboxes.
- A shipment has no Operation log entry: it is the platform's own work and no operation of an Application's.

### Reading: the operation `history`

Through the raw host interface:

```js
env.VM.invoke("history", { own: true, as: "user", schema: "public", table: "tasks",
                           key: { id: 7 }, limit: 100, before: "1234", values: "text" })
```

- `own` must be exactly `true` (there is no history of a shared database); `as` is as for `query`. `table` is a string; `schema` is a string and defaults to `public`; neither is longer than 63 bytes (no table is named so). `key`, `limit`, `before` and `values` are optional: `key` an object whose values are each `null`, a string, a number or a boolean; `limit` an integer from 1 to 1000, by default 100; `before` a string of decimal digits; `values` as for `query`. Anything else, `null` for any of them included, is `{ error: { kind: "malformed", message } }`, and nothing reaches any database. Arguments the operation does not have are ignored, as everywhere.
- It is decided as a `query` on the own database is: allowed for both actors whenever a request is in flight, refused `no-request` otherwise, refused `log-unavailable` when its Operation log entry cannot be written. It is not an operation of a transaction: it never takes a `transaction`, and what an open transaction has changed and not committed is not in it.
- Answer: `{ ok: { columns, types?, changes: [ ... ] } }`, **newest first**. `columns` are the table's columns as it is now, in its order (`types` their type OIDs, with `values: "text"`). Each change:

```json
{ "id": "1234", "at": "<RFC 3339, UTC>", "kind": "insert" | "update" | "delete" | "structure",
  "key": { "id": 7 } | null, "old": [ ... ] | null, "new": [ ... ] | null, "changed": ["title"] | null,
  "user_id": "...", "application": "...", "actor": "user" | "application" | "migration",
  "request": "..." | null, "migration": "..." | null, "structure": { ... } | null }
```

  `old` and `new` are rows in the order of `columns`, each value as `query` gives a value of that column (so as text with `values: "text"`). `changed` names columns by their names in the database.
- **Which changes.** Those of the table `schema`.`table` of the Application in flight, and of no other Application, whatever is passed. With `key`: the changes of rows recorded under that key, and every `structure` entry of the table. The key is compared as the table's key columns compare values, not as text: `7` and `"7"` name the same `integer` or `bigint` key, `"1.5"` the `numeric` key `1.50`, a `timestamptz` in any notation the same instant. A number or a boolean given for a `text` key is compared as its text. A `key` that names something other than exactly the table's primary key columns (one missing, another column, a name the table does not have, any key for a table without a primary key) answers no row's changes (the `structure` entries only); a value its column's type refuses is `{ error: { kind: "database", code } }` with Postgres's SQLSTATE. `before`: only changes with a lesser `id`. `limit` counts every change answered, `structure` entries too.
- **In the table's present shape**, as "Reading the Change history" says: read by the host in the own database, **as the runtime role, in a read-only transaction**. Code of the Application's that this runs (a domain's check, a type's input) runs with the runtime role's rights and can write nothing. A table that is not there now is `{ error: { kind: "database", code: "42P01" } }`; a value that no longer fits its column's type fails the read whole with that SQLSTATE.
- One correction to "Reading the Change history": **a `jsonb` (or `json`) column that held JSON `null` reads as JSON `null`**, not as SQL null. (With `values: "text"` that is the string `"null"` against `null`; without it, and in Application code through the SDK, both are `null`, as for `query`.) A `json` column reads as the capture recorded it: as `jsonb` does (key order, white space and duplicate keys are not kept).
- **It ships first.** The answer holds every change of that table committed, and its write answered, before the read began: in the same request or an earlier one. What it shipped is out of the outbox when the read answers.
- **Limits.** A read whose shipment and reading together take more than 10 seconds is ended: `{ error: { kind: "limit", limit: "time" } }` (the shipment goes on by itself; a later read finds it done). An answer of more than 8 MiB is `{ error: { kind: "limit", limit: "bytes" } }`. When the own database cannot be reached, or the shipment before the read fails otherwise than by taking too long: `{ error: { kind: "unavailable" } }` (when the platform database cannot, the entry cannot be written and the read is refused `log-unavailable`, like any operation). A read never answers part of what it was asked for.
- **In the Operation log**: one entry, written before anything is read and completed at the end: `operation` `history`, `own_database` true, `resource` null, the actor, `transaction` null, `statement` the text `history "<schema>"."<table>"` with the two names as given (no key, no limit, no value), `rows` the number of changes answered, `outcome` and `error` as for a `query`.

### Through the SDK

`history(table, key?, options?)` is as "Reading the Change history" describes it, with these changes:

- Each call is exactly one host operation `history` (no longer a `query`), with one Operation log entry as above.
- `Change<T>["kind"]` is `"insert" | "update" | "delete" | "structure"`, and a change has one more property, `structure`: `null` for a row's change; for `structure` an object (below), with `old`, `new`, `changed` and (in what the host answers) `key` null. Its type is exported as `StructureChange`: a union discriminated by `change`, each member with exactly the properties the table below gives it (`column`, `from`, `to`, `type`, `partition` strings; `columns` a `string[]`). `Change<T>` is narrowed by `kind`: for `"structure"`, `structure` is a `StructureChange` and `old`, `new` and `changed` are `null`; for the others `structure` is `null`.
- `id` is the change's id in `change_history`.
- The last paragraphs of "Only its own, and only to read" now read: the SDK has no call that writes, changes or deletes history; there is no table for an Application to declare. `vm.history` does not exist, and whatever an Application sends at the outbox (declared as a Drizzle table of its own, or through the raw interface) fails with `OperationFailed` of kind `database`, code `42501`, and changes nothing.

### Changes of structure that change data

Some changes a migration makes to a table that already has history change or remove data with no row inserted, updated or deleted. The platform sees each from the catalogs at the end of the migration that made it, before the Version's migrations are committed, and records it as an entry of that table's history: `kind` `structure`, `actor` `migration`, `user_id` the Contributor who sent the Version, `migration` the migration's name, `request` null, `key`, `old`, `new` and `changed` null, and `structure`:

| `structure` | When |
|---|---|
| `{ "change": "column-type", "column": "<name>", "from": "<type>", "to": "<type>" }` | A column's type was changed (with or without `USING`; the catalogs do not say whether values changed). The types as Postgres's `format_type` prints them (`integer`, `numeric(10,2)`, `character varying(20)`), a type that is not Postgres's own always with its schema (`public.mood`). |
| `{ "change": "column-dropped", "column": "<name>" }` | A column was dropped. |
| `{ "change": "table-dropped" }` | The table was dropped, and none is there under its name. |
| `{ "change": "table-replaced" }` | Another table is there under the name (the first dropped, or renamed away, and a new one made). |
| `{ "change": "table-rewritten" }` | Postgres wrote the table's rows anew and no `column-type` entry of the same migration shows why: a column's type "changed" to the type it had, with `USING` (which changes values), and also what rewrites without changing a value (`CLUSTER`, `SET LOGGED`, a column added with a volatile default). The catalogs do not tell these apart. A partitioned table has this entry when any of its partitions was written anew (its rows' changes are recorded under its name), and so has that partition. |
| `{ "change": "enum-value-renamed", "type": "<schema.type>", "from": "<value>", "to": "<value>", "columns": ["<name>", ...] }` | A value of an enum type was renamed; on every table with a column whose values hold values of that type: the type itself, an array, a domain, a range or multirange, or a composite type with such an attribute, of it or of any of these in turn, to any depth (a domain over a domain, an array of a domain, a composite type with an attribute that is an array of the enum). `columns` in the table's column order. |
| `{ "change": "partition-attached", "partition": "<schema.table>" }`, `{ "change": "partition-detached", "partition": "<schema.table>" }` | A table that was there when the migration began was attached to the table as a partition (one the migration makes with `CREATE TABLE ... PARTITION OF` brings no rows and has no entry); a partition was detached from it, or dropped. The entry is the parent's; the partition's own history has none. |

- Each entry has an `id` like any change, greater than those of the rows the same migration changed before it. It is in the same transaction as the migration: a Version that is refused or fails leaves none.
- **It is in every row's history of that table**: a read with a `key` answers it among that row's changes, in the order of `id`.
- A table has such entries from the end of the migration that made it: none for what that same migration did to it, and one for each such change a later migration makes, of the same Version or of a later one.
- A dropped table's entries (and its rows' earlier changes) stay in `change_history`; `history` cannot read them while no table is there under the name (`42P01`).
- Names of columns in `structure` are the database's, also through the SDK.
- For a row's change, `changed` also names a `json` or `jsonb` column that went from JSON `null` to SQL null or back: the two are different values.
- A change of structure that changes no data has no entry: a column or a table renamed (history is kept under the table's name at the time of each change, so a renamed table's earlier changes stay under its old name), a column added, a default, a constraint, an index, a table made to inherit from another (`INHERIT`: each table's rows stay its own).
- The rows a rewrite changed are recorded one by one, besides the `column-type` or `table-rewritten` entry: "A rewrite records each row it changed", below.

### What the roles of an own database can and cannot do

"What the runtime role cannot do" and "What a migration cannot do" hold with `vm.history` read as "the outbox", and with these changes: neither role can **read** the outbox (`42501`); the runtime role can read `vm.identity()` and `vm.ctx` only; a migration can no longer read the history at all. An Application's code reads its history through `history`, and only its own.

**Neither role can make an entry of the history other than by changing a row.** Whatever a migration or the runtime role calls in `vm`, with whatever arguments (the functions the platform itself calls to record a migration's changes of structure among them), `change_history` afterwards holds for that Application only the changes of rows that were made, under the identity they were made with, and the `structure` entries of changes of structure that were made, with the migration that made them. A Version whose migration tries is refused or fails, or applies with nothing forged; none leaves an entry for a change that did not happen, or under another User, migration or table.

### What this does not close

- The platform database grows with every change of every Application; nothing bounds an Application's share of it yet.
- Between a change's commit and its shipment the change is in the own database's outbox, where the platform's role, a superuser and whoever else holds the database's own rights outside the platform can reach it. No role a Contributor's code runs as can.
- A superuser of the Postgres instance can read and change `change_history`, as it can everything.

## A rewrite records each row it changed (ticket: A migration that rewrites values records each changed row)

ADR 0017, third slice. A migration can change every value of a table with no row trigger firing: `ALTER TABLE ... ALTER COLUMN ... TYPE ... USING <any expression>` makes Postgres write the table's rows anew. From this ticket on, each row such a statement changed is an entry of the history, like a row an `UPDATE` changed.

### When

- **A rewrite** is a statement of a migration after which a table has a new file node: Postgres wrote its rows anew. It is seen from the catalogs, not from the statement's text, so it is recorded the same however the statement does it: `ALTER TABLE` itself, dynamic SQL in a `DO` block, a function or a procedure the statement calls.
- It is recorded for a table that had history when the migration began (it was made by an earlier migration, of this Version or of an earlier one) and is still there after the statement. A table the same migration made has no such entries, as it has no `structure` entries.
- A partitioned table's rows are in its partitions: when a statement writes a partition anew, the rows are recorded under the name of the partitioned table at the top, where the Application reads them. "At the top" among the tables that had history when the migration began: when the migration itself puts such a table under a new partitioned table and rewrites it through that one, the rows are recorded under the table's own name. A partition the migration itself made under a partitioned table with history is part of that table: when a statement writes it anew, also the statement that made it, the partitioned table's rows are recorded as above. So is one made at any depth under it (a partition of a partitioned table the migration made under the table with history), and so it is whatever the own database's server has `track_counts` set to. A partition the migration only makes or attaches records no row: only a statement that writes rows anew does. A table with history that the migration attaches under a partitioned table with history is part of that table from then on: when a later statement writes it anew, its rows are recorded under the partitioned table's name. A partition the migration detaches is a table of its own from then on: when a later statement writes it anew, its rows are recorded under its own name. A table that inherits from another (`INHERITS`) is a table of its own: a statement that retypes a column of the parent writes both anew, and each has its own rows' entries under its own name (the parent's rows are those it holds itself, not its children's).

### What is recorded

The platform takes the table's rows before the statement and after it, and pairs them by primary key.

- **A row whose value differs** is one entry: `kind` `update`, `actor` `migration`, `user_id` the Contributor who sent the Version, `migration` the migration's name, `request` null, `key` the row's primary key after the statement (of a `delete`, below, the key it had before), `old` the row before the statement and `new` the row after it, each whole and as the capture records a row, `changed` the columns whose values differ, by their names after the statement, in the table's column order.
- **Which columns are compared**: those the table has both before and after the statement. A column the statement renamed is the same column. A column it added is in `new` and not in `changed`; one it dropped is in `old` and not in `changed` (the `column-dropped` entry says so).
- **When a value differs**: when its JSON form in `old` and in `new` are different JSON values. Numbers are compared by value: `12` as `integer` and as `bigint`, and `1.50` and `1.5000`, are the same; a number and a string are different (`5` and `"5"`), and so are a time without and with its offset. For a `json` or `jsonb` column, JSON `null` and SQL null are different, as for any change.
- **A row whose value does not differ has no entry.** A change of type that changes no value records no row and is not refused: `integer` to `bigint`, `varchar(50)` to `text`, `numeric(10,2)` to `numeric(12,4)`, and whatever writes rows anew without changing them (`SET UNLOGGED`, a column added with a volatile default).
- **The same row** before and after is the one with the same primary key: each key column equal as JSON values, or one a number or a boolean and the other the string that reads the same (so a key retyped from `integer` to `text` still pairs). A row before with no row after under its key is an entry of `kind` `delete` with its `old`; a row after with none before is one of `kind` `insert` with its `new`; both with `actor` `migration` and the rest as above. A statement that gives every row a new key is so recorded as each row deleted and each inserted.
- The entries are in the migration's transaction: a Version that is refused or fails leaves none. Their `id`s are greater than those of the rows the same migration changed before the statement and of the rows the statement itself changed (an `UPDATE` in the same `DO` block), and less than those of what the migration changes after it and of its `structure` entries. Their order among themselves is not promised.
- They are read as any change is: `history` with the row's `key` answers them among that row's changes.
- The row before is the row as it was before the statement. A statement that both changes rows with `UPDATE` and rewrites the table (a `DO` block) has its `UPDATE`s recorded by the capture as always, and the rewrite's entry on top, with `old` the row before the statement. A row the statement itself inserted before it wrote the table anew was not there before the statement: it has the capture's `insert`, with the row as it was put in, and the rewrite's `insert`, with the row as the statement left it.

### A table without a primary key

Rows without a key cannot be paired. When a rewrite leaves such a table with other rows than it had (compared as the rows are recorded, over the columns it has both before and after, each row as many times as it is there), the Version is refused: `migration-refused`, rule `rewrite-without-key`, the message names the table and says to give it a primary key in an earlier statement. When the rows are the same, nothing is recorded and the Version applies. A table whose primary key is not the same columns before and after the statement is treated so too; one whose primary key was dropped and made again on the same columns in the statement is paired by it as any other.

### The reading cannot be made to lie

The platform reads the rows on the migration's own session, as the migration role, never with its own rights: reading a table can run code (a policy, a cast), and it would run with the reader's rights. What could make that reading show other rows or other values than the table holds is refused, or does not matter:

- **Row security.** When row security would apply to the migration role's reading of the table, before or after the statement (`FORCE ROW LEVEL SECURITY`, with a policy or without), the Version is refused: `migration-refused`, rule `rewrite-row-security`, the message names the table and says to take `FORCE ROW LEVEL SECURITY` off before the statement and put it back after. Row security that is enabled and not forced does not apply to the table's owner, and is not refused.
- **A cast to `json` or `jsonb`** from one of the Application's types whose function is not Postgres's own, when it exists at a rewrite's reading, is `migration-refused` with rule `json-cast`, also when the migration drops it again before its end. (A rewrite is not a change the capture sees, so this is not the `VM003` of a row changed while such a cast exists.)
- **A virtual generated column** (Postgres 18) is null in `old` and in `new`, as in the capture, and never in `changed`; the stored columns of such a table are recorded as of any other.
- **Nothing a migration calls in `vm`** changes what is recorded: "Neither role can make an entry of the history other than by changing a row" holds with the rows a rewrite changed counted among the rows changed. Whatever a migration calls, with whatever arguments, before, in or after the statement that rewrites, the history afterwards holds for each rewritten row what it was before the statement and what it is after, and no entry for a row the rewrite did not change.

### A statement may run more than once

To have the rows before, the platform must know that a statement rewrites before it runs it. It may therefore run a Version's migrations, or some of their statements, more than once, each earlier run undone before the next. A Version none of whose statements rewrites a table with history is run once.

- What stays of a Version is one run's work: a row a migration inserts before its rewrite is there once, and once in the history. What Postgres does not undo (a sequence's values) may show the earlier runs.
- At most 5 runs. The platform remembers every table a statement was seen to rewrite, in whichever run, and takes the rows of all of them before it: a statement that rewrites one of two tables in turn settles by the third run and is recorded as the run that stayed. A Version with a statement that in its fifth run still rewrites a table with history it had rewritten in none before (so that the platform has not that table's rows before) is refused: `migration-refused`, rule `rewrite-unsteady`, the message names the migration and the tables that statement was seen to rewrite.
- All runs together, with the reading and the comparing, are held to the time a Version's migrations have.

### What this does not close

- A change of type that Postgres makes without writing the rows anew records no row: the stored values are the same. For `timestamp` to `timestamptz` while the session's time zone is UTC, the JSON form of the same instant gains its offset from then on; the `column-type` entry is what says so.
- A column or a table dropped, a table replaced under its name, an enum's value renamed and a partition detached are one `structure` entry each, not an entry per row: the rows' last values are in the history already.
- Every changed row of a rewrite is a row of `change_history`: a rewrite of ten million rows that changes each writes ten million, and nothing bounds an Application's share of the platform database yet.


## The tool (ticket: The tool signs in, and lists Resources and Applications)

The same program, run on a Builder's machine by their Coding agent (ADR 0006, ADR 0015). Its first reader is an agent: every command has a machine-readable form, exit codes that say what kind of ending it was, and messages that say what to do next.

### Every command

- **Exit codes.** `0`: done. `1`: failed (the installation cannot be reached, it answered something unexpected, a file cannot be read or written). `2`: the command line is wrong. `3`: refused, or waiting for the Builder (the platform said no, or the next step is a person's: the message says whose and what). `4`: not signed in (no credential, or one that has expired or was revoked): sign in and run the command again. `vm --help` lists these five, each with its number and meaning.
- **`--json`**, on every command: standard output is exactly one JSON object and a line end, nothing else. A command that does not end with `0` prints `{ "error": { "reason": "<kebab-case>", "message": "<a sentence a Coding agent can act on>" } }` there (for exit code `2` the usual usage text on standard error is enough). Without `--json`, the result is text on standard output and an error is its message on standard error.
- Reasons the tool itself gives: `not-signed-in` (4), `sign-in-expired` (4), `sign-in-refused` (3), `unreachable` (1), `unexpected-answer` (1), `credential-file` (1: the credential file or its folder cannot be read, written or made, or the file does not hold what the tool wrote there; the message names the path). A refusal of the platform's API (an answer of `4xx` other than `401`) keeps the API's own `reason` and `message` and ends with `3`; an answer of `5xx` keeps them too and ends with `1`: the platform could not do it now.
- Every wait has an end: an installation that does not accept the connection within 5 seconds, or does not answer within 30, is `unreachable`. An answer whose body is larger than 8 MiB is `unexpected-answer`.
- **Without `--json`, nothing the installation answered reaches the terminal raw**: a control character in a name, a description or any other text of the installation's (also a line end, and the escape character that begins a terminal's control sequence) is printed as `\u{<hex>}`, so that what one User wrote cannot move the cursor, recolour or hide text on another's terminal. The same for the characters that reorder text without being control characters: the bidirectional marks and overrides U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. With `--json` each of these is written as a JSON escape (`\u001b`, `\u202e`), so the output holds none raw and still parses to the original text.
- A command whose standard output is closed before it has written (`vm resources | head -0`) ends without a crash: with the exit code it would have had, or `1`, and nothing but an ordinary message, if anything, on standard error. A closed standard error is no crash either.
- The messages for `not-signed-in` and `sign-in-expired` say to run `vm sign-in`, and `sign-in-expired` gives the whole command with the installation it was signed in to.

### Signing in

```
vm sign-in --domain <domain> --connect <ip:port> --stand-in-user <id> [--stand-in-groups g1,g2] [--json]
vm sign-out [--json]
vm whoami [--json]
```

- `vm sign-in` asks the installation for a **tool credential** for that User and keeps it on the Builder's machine. While sign-in is the stand-in, the User and their Groups are stated on the command line (`--stand-in-user` is required); the ticket on signing the tool in through the browser replaces those two options and nothing else here.
- Until the ticket on installing brings HTTPS, the tool speaks plain HTTP and only to the address given with `--connect`, which must be a loopback address (`127.0.0.1:<port>`, `[::1]:<port>`); another address is exit code `2`. It sends `Host: api.<domain>`.
- It prints the User, their Groups and the installation: `{ "user": "<id>", "groups": [...], "platform_administrator": <bool>, "domain": "<domain>", "expires_at": "<RFC 3339 time>" }`. An installation that does not accept the sign-in (the stand-in is off) is `sign-in-refused`, exit code `3`, and nothing is kept.
- Signing in again replaces the credential kept; the earlier one is ended at the installation if it can be reached. A sign-in whose credential cannot be kept (the file cannot be written) is `credential-file`, and the credential just made is ended at the installation: none is left alive that no file holds.
- `vm whoami` asks the installation who the credential is: `{ "user", "groups", "platform_administrator", "domain" }`. The Groups are those of the sign-in.
- `vm sign-out` ends the credential at the installation and removes it from the machine: `{ "signed_out": true, "ended_at_installation": <bool> }`, exit code `0`. If the installation cannot be reached the credential is removed from the machine all the same (`ended_at_installation` false). A credential the installation says has ended already counts as ended there (`ended_at_installation` true). With nothing to sign out of: `{ "signed_out": false, ... }`, exit code `0`.

### The credential

- It is kept in the user's own configuration folder: `$XDG_CONFIG_HOME/vm/` when that variable holds an absolute path, else `$HOME/.config/vm/` (Linux and macOS; Windows comes with the ticket on the three systems). The tool makes the folder with mode `0700` if it is not there, and writes the file with mode `0600` whatever mode it had. After a sign-in the folder holds one file, a text file with the credential in it; its name and the rest of its form are not promised.
- **It is never printed**: not on standard output or standard error of any command, with or without `--json`, not in an error message, and it is not taken from the command line or the environment. It is not in the program's log on the installation's side, and the platform database does not hold it in a form that can be used (it keeps a digest).
- It is 32 random bytes or more, made by the installation, and cannot be guessed. Its text begins with `vmtc_`, so that one found where it should not be can be recognised.
- It ends when its time is up, when it is signed out, and when its User makes a seventeenth: a User holds at most 16 live credentials (one a machine), and making one more ends the one made longest ago. Never otherwise in this ticket. Its lifetime is 30 days from the sign-in; `vm serve --tool-credential-lifetime <seconds>` sets another (tests use a short one).

### On the platform's API

- `POST /v1/tool-credentials`, by a signed-in User who signed in otherwise than with a tool credential (for now: the stand-in), with an empty body or `{}`: `201` with `{ "credential": "<the credential>", "user": "<id>", "groups": [...], "platform_administrator": <bool>, "expires_at": "<RFC 3339 time>" }`. This is the only answer that ever holds a credential. Asked with a tool credential it is `403` with reason `not-with-a-tool-credential`: a credential cannot renew itself.
- `Authorization: Bearer <credential>` on a request to `api.<domain>` is a sign-in like any other, as the User and the Groups of the moment the credential was made: every request of the API may be made with it. A request with an `Authorization` header is decided by it alone: a credential that is unknown, expired or signed out is the usual `401` (the same plain body as for anyone not signed in), whatever stand-in headers come with it; malformed stand-in headers are still `400` first.
- A tool credential is not a sign-in on an Application's address: there the header counts for nothing, and the request is `401` unless signed in otherwise. An `Authorization` header that holds a tool credential, or what looks like one (`Bearer vmtc_...`, the scheme in any letter case; by the prefix alone, alive or not, so nothing is looked up), is never handed to an Application's code: the request reaches it without that header, so a credential sent to the wrong address by mistake is not an Application's to use. Any other `Authorization` header is handed on as before.
- `DELETE /v1/tool-credentials/current`, with the credential: `204`, and the credential is ended at once. Signed in otherwise: `404` `not-found`. Any other method on the two paths is `405` `method-not-allowed`.
- `GET /v1/me`, by any signed-in User: `{ "user": "<id>", "groups": [...], "platform_administrator": <bool> }`.
- The `401` for a credential that has ended does not say whether it expired, was signed out or never existed. The tool calls all three `sign-in-expired`.

### Listing Resources

```
vm resources [--json]
```

- `GET /v1/resources` and `GET /v1/resources/<name>` gain, in each Resource, `"your_access": "structure" | "read" | "write" | null`: the highest Access level the User holds on it through their Groups, null when they hold no Grant.
- `vm resources --json` prints `{ "resources": [ { "name", "kind", "description", "owner", "contact", "your_access" } ] }`, by name: **every** registered Resource, also those the User holds no Grant on.
- Without `--json`, one block per Resource with the same facts in words. For a Resource with no Grant it says so and says whom to ask, with the Resource's `contact` (`no access: ask <contact>`); for one held at `structure` it says that rows cannot be read at that level.
- It shows no structure: no table, no column, nothing of what is inside a Resource, and nothing of its connection.
- With no Resource registered: `{ "resources": [] }`, and in words that none is registered and that a Platform administrator registers them.

### Listing Applications

```
vm applications [--json]
```

- `GET /v1/applications`, by any signed-in User: `{ "applications": [ { "name", "description", "address", "contributor": <bool>, "serving_version": <number or null> } ] }`, by name: the Applications the User contributes to or can open. `address` is `<name>.<domain>`.
- **Until the ticket on Contributors and sharing says who can open an Application, the list holds the Applications the User is a Contributor of** (`contributor` true), and no other. That ticket adds the ones shared with them.
- `vm applications --json` prints the same object. Without `--json`, one block per Application: name, description, address, and whether a Version is serving.
- With none: `{ "applications": [] }`, and in words that the User contributes to no Application yet.

## Pulling a Resource's structure (ticket: Pull a Resource's structure as a typed Drizzle definition)

The Structure puller (ADR 0003, ADR 0012, ADR 0014). The platform reads a shared database's catalogs and answers a Drizzle definition and a fingerprint; the tool writes them into the project.

### The structure login

- `POST /v1/resources` takes, in `connection`, an optional `"structure": { "role": "...", "password": "..." }`, beside `read` and `write`. `null` is the same as none; anything else that is not an object with those two strings is `400` `malformed`. Like every other part of `connection` it is in no answer, log record or error message.
- A pull connects with the structure login when the Resource has one, and with the read login otherwise. It never uses the write login.

### What a pull reads

- **Catalogs only.** Every statement the puller sends is a fixed text of the program's, with values as parameters, that reads from `pg_catalog` and nothing else: no table, view or function of the shared database's own, no row of any of them, and no statistics (`pg_statistic`, `pg_stats`, `pg_stat_*`, `pg_class.reltuples`). It runs in a read-only transaction and changes no setting but its own session's (`search_path`, a statement timeout, the transaction's read-only state). With statement logging fully on in the shared database, the server log shows it.
- **At most 2 pulls of one Resource run at once.** A third is answered at once with `503` `structure-unavailable` (the message says pulls of this Resource are running and to try again shortly; the Operation log entry's `error` is `busy`) and sends nothing. So pulls, which may share the read login with Applications, never hold more than two of its connections, and an Application's operations on the Resource are served while pulls run.
- A pull is held to 10 seconds in all, and its answer's body to 4 MiB. Past either: `503` `structure-unavailable` (the Operation log entry's `error` is `time`) or `422` `structure-too-large` (`bytes`).

### On the platform's API

`GET /v1/resources/<name>/structure`, by a signed-in User:

```json
{ "resource": "devops", "fingerprint": "sha256:<64 hex>", "your_access": "structure" | "read" | "write",
  "schema_ts": "<the definition, as text>",
  "unsupported": [ { "schema": "public", "table": "sites", "column": "area", "type": "polygon" } ] }
```

- Allowed for a User who holds a Grant on the Resource at any level (`structure` is enough). The decision is the Authorization decision's, with `structure` needed.
- No Grant: `403` with reason `no-grant`; the message names the Resource's `contact` as whom to ask and says not to build against the Resource until then. Beside `reason` and `message` the error has `"resource"` and `"contact"`. Nothing is sent to the shared database.
- No such Resource: `404` `not-found`. A shared database that cannot be reached, refuses the login, or does not answer in time: `503` `structure-unavailable`, with nothing about the connection in it.
- Every pull, allowed or refused, has one entry in the Operation log: `operation` `structure`, `application` and `version` null, `request` the request's id, `user` the User, `resource`, `statement` null, `outcome` `allowed`, `refused` (`reason` `no-grant`) or `failed` (`error` `unavailable`, `time` or `bytes`), `rows` null. It is written before anything is sent to the shared database; if it cannot be written the pull is `503` `unavailable` and nothing is sent.

### What is in the structure

- Every ordinary table, partitioned table, view and materialized view of every schema except Postgres's own (`pg_catalog`, `information_schema`, `pg_toast`, and any `pg_*`), whatever the pulling login may do with them. A partition is not listed apart from its partitioned table. Tables of an extension are listed like any other.
- Of each: its schema and name; its columns in order, each with its type, whether it may be null, whether it has a default (and the default's expression), whether it is an identity column (always or by default) or a generated column; of a table also its primary key, its unique constraints and its foreign keys (each with its name; columns, the table and columns referred to, `ON DELETE` and `ON UPDATE`).
- Every enum type a listed column uses, with its labels in order.
- Not in the structure: indexes that back no constraint, check constraints, triggers, functions, privileges, row security, comments, sequences, owners, storage. Changing one of these changes neither the definition nor the fingerprint.

### The fingerprint

- `sha256:` and 64 hex digits, over the structure as listed above and nothing else: the same structure gives the same fingerprint, whichever login pulled it, on whichever server of the same major version of Postgres, however and in whatever order its tables were made (object ids and creation order are not in it). A default's or a generated column's expression is in it as Postgres prints it, which another major version may print otherwise.
- Any change to what is listed gives another: a table, view, column, enum label, key or foreign key added, dropped or renamed; a column's type, nullability, default, identity or generation changed; a column moved.

### The definition

`schema_ts` is one TypeScript module for `drizzle-orm/pg-core` (0.45 or later, below 1.0) that type-checks under the project's `tsconfig.json`:

- Its first lines are `// Generated by vm pull. Do not edit.` and `// Resource: <name>  Fingerprint: sha256:<64 hex>`.
- A table of schema `public` is `export const <name> = pgTable("<table>", { ... })`; one of another schema hangs off `export const <schema>Schema = pgSchema("<schema>")`. A view is `pgView("<view>", { ... }).existing()`, a materialized view `pgMaterializedView(...).existing()`; of another schema, `<schema>Schema.view(...)` and `<schema>Schema.materializedView(...)`. An enum is `pgEnum`, or `<schema>Schema.enum`.
- **Export names.** A table, view or enum of `public` is exported under its name in camel case (`incident_notes` → `incidentNotes`); one of another schema under the schema's name and its own, in camel case (`crm.accounts` → `crmAccounts`). A column's key is its name in camel case; the column's real name is always given. Camel case: the name is cut at every run of characters other than ASCII letters and digits, each piece after the first gets its first letter in upper case, and the pieces are joined; nothing else changes case (`stockLevels` stays as it is, `per-user code` → `perUserCode`). What then begins with a digit, or is empty, gets a `_` in front (`2fa codes` → `_2faCodes`). Where the result is a reserved word of TypeScript, or meets one already given (exports among the module's exports, keys among their table's keys; taken in the order of schema, then name, as Postgres sorts them in `C` collation, columns in the order of their names), `_2` is added, or `_3`, ...: the first that is free. The same rule for exports and for column keys; the result depends on the structure alone. The names the module itself uses are taken before any other (`sql`, so a table `sql` is `sql_2`), then the schemas' exports (`<schema>Schema`), then tables, views and enums.
- **Types expressed**: `smallint`, `integer`, `bigint` (Drizzle's `bigint` with `mode: "number"`, as `drizzle-kit pull` writes it), `smallserial`, `serial` and `bigserial` where a column is one, `boolean`, `text`, `varchar(n)`, `char(n)`, `numeric(p,s)`, `real`, `double precision`, `date`, `time` with and without time zone, `timestamp`, `timestamptz` (with their precision), `interval`, `uuid`, `json`, `jsonb`, an enum, and an array of any of these (any number of dimensions as one). `date`, `timestamp` and `timestamptz` are in Drizzle's `mode: "string"`: the value is the text Postgres prints, as the host hands it over.
- A table whose foreign keys refer to itself, and two tables whose foreign keys refer to each other, type-check like any other. A foreign key to a table that is not listed (a partition) is in the fingerprint and not in the definition. An enum with no label is a type it cannot express.
- **A type it cannot express** (`bytea`, a range, a geometric or network type, a domain, a composite, an extension's type, ...) is not dropped: the column is in the definition as a custom type whose TypeScript type is `unknown` and whose database type is the type's name, and it is listed in `unsupported` with its `type` as Postgres prints it with nothing on the search path (`polygon`, `int4range`, `public.positive_int`). Its value reads as the text Postgres prints.
- A primary key of one column is `.primaryKey()` on it, of several a `primaryKey({ columns: [...] })`; `NOT NULL` is `.notNull()`; a default is `.default(sql\`<expression>\`)`; identity is `.generatedAlwaysAsIdentity()` or `.generatedByDefaultAsIdentity()`; a generated column `.generatedAlwaysAs(sql\`<expression>\`)`; unique constraints and foreign keys are in the table's extra config (`unique(...)`, `foreignKey({ ... })` with its actions). So an `insert` type-checks without the columns that have a default, and not without a required one.
- The same structure gives the same text, byte for byte (under another Resource's name only the second line differs): tables by schema then name, as Postgres sorts them (`C` collation); columns in their order.
- No relations are generated in this ticket: `resources/<name>/relations.ts`, when a project has one, is the Builder's.

### The tool

```
vm pull <resource> [--project <directory>] [--json]
```

- The project is `--project`, or the current directory. A directory without `manifest.ts` is not one: reason `not-a-project`, exit code `1`, nothing written.
- It asks the installation for the structure and writes two files: `resources/<name>/schema.ts` (the definition, as answered) and `resources/<name>/index.ts` (the generated link of "A project", with the `relations` lines exactly when `resources/<name>/relations.ts` is there). It makes the folders it needs. Each file is written whole or not at all.
- It prints `{ "resource", "fingerprint", "your_access", "files": [ { "path": "resources/<name>/schema.ts", "changed": <bool> }, ... ], "unsupported": [ ... ] }`, exit code `0`. `path` is relative to the project. Without `--json`, the same in words; each unsupported column is one line that names the table, the column and the type, and says the column is typed `unknown`.
- **Pulling twice changes nothing**: the second pull of an unchanged structure writes no file (each `changed` is false, the files' bytes and modification times are as they were).
- A file at one of the two paths that does not begin with its generated first line (`// Generated by vm pull. Do not edit.` for `schema.ts`, `// Generated. Do not edit.` for `index.ts`) was not written by the tool: neither file is written; reason `not-generated`, exit code `1`, the message names the file.
- **Refused** (`no-grant`): exit code `3`, the API's reason and message (whom to ask; do not build against the Resource), and nothing is written or changed, also when an earlier pull's files are there.
- Not signed in, an installation that cannot be reached, and the other endings are as for every command.
- A Resource's name is checked by the tool before it is used in a path: what is not a Resource's name as the API takes one is exit code `2`.
- The tool writes inside the project only: when `resources` or `resources/<name>` is a symbolic link, or `schema.ts` or `index.ts` there is one, nothing is written anywhere; reason `not-a-project`, exit code `1`, the message names the link.

## A project: the template, creating one and checking one (ticket: The project template, and creating and checking a project)

What a Coding agent starts from (ADR 0006), and the one command that says whether a project will deploy.

### Creating a project

```
vm new <name> [--directory <directory>] [--json]
```

- `<name>` is an Application's name as the API takes one; anything else, or one of the platform's own names, is exit code `2`.
- The project is made in `--directory`, or in `./<name>`. A directory that is there and not empty is reason `directory-not-empty`, and a path that is there and is no directory is reason `not-a-directory`: exit code `1`, and nothing is written. An empty directory is used; a missing one is made (with its parents). A file that cannot be written is reason `project-file`, exit code `1`, and the message names it.
- It needs no installation and no sign-in, and runs nothing but itself: no `npm`, no `git`, no network.
- It prints `{ "project": "<absolute path>", "name": "<name>", "files": [ "<path relative to the project>", ... ], "next": [ "<command>", ... ] }`, the files in the order of their paths; `next` is what to run now, in order, the first being `npm install`. Without `--json`, the same in words.
- The same program makes the same project: every file's bytes depend only on the program and `<name>`.

### What a new project holds

```
AGENTS.md                 the instruction file for the Coding agent (below)
CLAUDE.md                 one line: @AGENTS.md
manifest.ts               defineManifest({ name: "<name>", description: "...", resources: {} }), a plain literal
src/index.ts              defineApp(manifest, { fetch }) answering one page that greets the signed-in User by id
src/db/schema.ts          the own database's Drizzle schema, with no table yet
migrations/               empty but for `.gitkeep`, which keeps the folder
vm.dev.json               the test Users file: one User, `outsider`, in no Group, and no Grant
package.json              private; dependencies @vm/sdk, drizzle-orm; devDependencies typescript, drizzle-kit, esbuild; exact versions
tsconfig.json             strict; "paths": { "@vm/own-structure": ["./src/db/schema.ts"] }; includes manifest.ts, src, resources
.gitignore                node_modules/, and what is in the tool's own folder .vm/ but for .vm/packages/ (git keeps the tarball)
.vm/packages/vm-sdk-<version>.tgz   the SDK this program carries, which package.json names by this path
```

- The SDK is not fetched from anywhere: the program carries it and writes it into the project, and `package.json` depends on that file (`"@vm/sdk": "file:.vm/packages/vm-sdk-<version>.tgz"`). So `npm install` needs the registry (or its cache) only for the other packages. Their versions are exact and are those of the repository's own lock file, which from this ticket on has `drizzle-kit` too: what a project installs is what the gate tested.
- **A new project installs and passes the check untouched**: `npm install`, then `vm check`, ends with `0`, with no problem, no warning and no note.
- **A new project is an Application as it stands**: built with the SDK's tooling and sent as a Version, it answers `GET /` with `200`, `text/html`, and a page that has the User's id, written as text of the page (escaped for HTML).
- `migrations/` of a new project holds `.gitkeep` and nothing else, and is in step with the schema, which has no table.
- `vm.dev.json` is `{ "about": "<what the file is for>", "users": [ { "id": "outsider", "displayName": "Outsider (no access)", "email": "outsider@example.com", "groups": [] } ], "grants": [] }`. The local loop's tickets say how it is used.

### The instruction file

`AGENTS.md` is written for the Coding agent, in plain words, at most 120 lines. It says what an Application is and how it reaches data (`ctx.user().db(name)`, `ctx.user().ownDb()`, `ctx.app().ownDb()`), that access belongs to people and a refusal for a missing Grant is not a bug to work around, and how to work: look for an existing Application first, find the data, keep the Application's own data in its own database, check, deploy. It names only commands this program has: every `vm <command>` in its code spans and code blocks is one that `vm --help` lists, `vm pull` and `vm check` among them, and each later ticket that adds a command adds it to the file.

It carries the three corrections of `docs/research/agent-facing-discovery.md`:

- **Data loss.** A migration that destroys data is never confirmed by the agent on its own: it tells the Builder exactly what would be lost and asks, **unless the Builder has already clearly said that this data may be lost**, in which case it goes on and says what was lost. The file has the words `unless the Builder has already clearly said that this data may be lost`.
- **The test User with no access.** The file says `vm.dev.json` holds a test User, `outsider`, in no Group and so with no Grant, and tells the agent to check what that User gets before it calls the work done. The file names `vm.dev.json` and `outsider` in one paragraph.
- **Who can see a new Application.** The agent is told to report who can see a new Application **from the tool's own output**, naming the people or Groups the tool named, and never from its own guess. The file has the words `from the tool's own output`.

And one rule the trial showed is needed: when `vm pull` is refused for want of a Grant, the agent stops and tells the Builder which Resource, what for, and whom to ask; it does not invent the Resource's tables, mock its data or quietly drop the feature. This is one paragraph of the file, which names `vm pull` and the Builder and has the words `refused`, `ask`, `invent` and `mock`.

### Checking a project

```
vm check [--project <directory>] [--timeout <seconds>] [--json]
```

The project is `--project` or the current directory; one without `manifest.ts` is `not-a-project`, as for `vm pull`. The check needs the tool signed in (it asks the installation which Resources are registered and what the Builder holds), Node 22 or later on the `PATH`, and the project's packages installed. It writes nothing in the project, not even for a while: no file or folder of the project is made, changed or removed, `migrations/` included. It makes four checks, each from what is on the machine now, and reports every problem it finds, not only the first:

| `check` | What must hold | `reason` when it does not |
|---|---|---|
| `manifest` | `manifest.ts` is a plain literal of the right shape (as the SDK's tooling reads it, without running it) | the tooling's own: `manifest-missing`, `manifest-not-literal`, `manifest-invalid`, with `file`, `line`, `column` |
| `resources` | every Resource the Manifest declares is registered on the installation | `resource-not-registered` |
| | and is pulled: `resources/<name>/schema.ts` and `resources/<name>/index.ts` are both there, each beginning with the first line `vm pull` gives it | `resource-not-pulled` |
| `migrations` | `migrations/` is in step with `src/db/schema.ts`: generating migrations now would write no migration (no `.sql` file; the journal `drizzle-kit` keeps beside them is not one). `migrations/` is as `drizzle-kit generate --dialect postgresql --schema src/db/schema.ts --out migrations` leaves it, or holds no migration at all. A change `drizzle-kit` asks a question about and writes nothing for (a renamed column) is not in step either | `migrations-behind`; and `own-schema-failed` when `drizzle-kit` cannot load `src/db/schema.ts` (it is not there, it throws, it imports what is not there): the message has the tool's own words and says to correct the file; and `migrations-unreadable` when `drizzle-kit` cannot read `migrations/` itself (a journal or a snapshot that is not as it wrote them): the message has the tool's own words, and says that migrations are never edited by hand and to bring the folder back as it was last committed |
| `types` | the project type-checks (`tsc --noEmit` with the project's own `tsconfig.json` and TypeScript) | `type-error`, one problem per error, with `file`, `line`, `column` and the compiler's message; at most 50, and then one last problem of the same check and reason, with `"more": <how many more>` and no `file`, `line` or `column`, whose message says how many more |

- **Every problem's message names the fix**: for `manifest-not-literal`, what was found and that the Manifest must be a plain literal because the platform reads it without running code; for `resource-not-registered`, that no such Resource is registered, the names that are, and to correct the name or ask a Platform administrator; for `resource-not-pulled`, the command `vm pull <name>` (and, when a file is there that the tool did not write, which file, and that it is to be moved away first); for `migrations-behind`, that `src/db/schema.ts` changed since the last migration and how migrations are generated (the ticket on generating migrations gives the command; until then the message says to generate one with `drizzle-kit generate`); for `type-error`, the file and line (an error the compiler gives with no place, such as a `tsconfig.json` that is not there, is a `type-error` whose `file`, `line` and `column` are null).
- **A warning does not fail the check.** When the Builder's own Access level on a declared Resource is below the declared one (`structure` or none against `read`; `read` against `write`), the check warns: reason `own-grant-below-declared`, with `resource`, `declared`, `held` (`"structure"`, `"read"`, or `null` for no Grant) and `contact`; the message says that the Builder, and every User whose Groups hold no Grant at that level, will be refused that Resource's operations by the Application, and whom to ask.
- **A note says what is worth knowing.** A Resource that is pulled (a folder `resources/<name>/` whose `schema.ts` begins with the first line `vm pull` gives it) and not declared in the Manifest is a note: reason `pulled-not-declared`, with `resource`; the message says to declare it or remove the folder.
- With `--json`, standard output is one object: `{ "ok": <bool>, "problems": [ { "check", "reason", "message", ... } ], "warnings": [ { "reason", "message", ... } ], "notes": [ { "reason", "message", ... } ] }`, and when `ok` is false also `"error": { "reason": "check-failed", "message": "<how many problems, and the first>" }`, as every command that does not end with `0` has. Without `--json`, the problems, warnings and notes in words: on standard output when the check passes, on standard error when it fails. Text of the installation's and of the project's files is printed as the tool prints any text that is not its own.
- **Exit codes**: `0` when there is no problem (warnings and notes or not); `1` when there is a problem (`check-failed`); `4` not signed in; and as for every command otherwise. What is looked at first, in this order: the command line (`2`), the project (`not-a-project`), the sign-in (`4`), then `node` (`node-missing`), then the packages (`not-installed`). What the check itself needs and does not find is exit code `1` with its own reason and the fix: `node-missing` (no `node` on the `PATH`, or older than 22), `not-installed` (the project's packages are not installed: run `npm install`; decided by whether `@vm/sdk`, `typescript` and `drizzle-kit` resolve from the project as Node resolves them).
- **The check has an end**: 300 seconds in all, or `--timeout <seconds>` (a whole number from 1 to 3600; anything else is exit code `2`). Past it the check ends with exit code `1`, reason `check-timed-out`, and the message names the check it was in. What it starts it also stops, and what those started in their turn: each tool runs in a process group of its own, and nothing of the group is left running when the check ends, in time or not. (A process that leaves its group on purpose is not followed; the check runs the Builder's own project as the Builder.)
- **A check that is interrupted** (`SIGINT`, `SIGTERM` or `SIGHUP`, as a Coding agent's harness sends when it gives up on a command) ends the same way: it stops what it started, and ends with exit code `1`, reason `interrupted`.
- What the check needs to write, it writes in a folder of its own (mode `0700`) in the system's temporary folder (`TMPDIR`), and removes it in every ending: passed, failed, out of time, interrupted. This holds too for a project whose `tsconfig.json` has the compiler keep build information (`incremental`, `composite`): none is written in the project.
- The check runs the project's own tools (`tsc`, `drizzle-kit`, the SDK's tooling), each found as Node resolves its package from the project and run with the `node` on the `PATH` (`typescript`'s `tsc` and `drizzle-kit` by the `bin` of their `package.json`; the SDK's `tooling/main.js`), and so whatever `src/db/schema.ts` imports: as the Builder, on the Builder's machine, like any build. It does not run `src/index.ts` or the Manifest, and it reads no `drizzle.config.*`: the schema is `src/db/schema.ts` and the migrations are in `migrations/`, always.
- When the Manifest cannot be read, the `resources` check has nothing to go by and is left out; the other checks are still made.
