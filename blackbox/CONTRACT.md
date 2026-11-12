# The outside of the `vm` program

What the black-box tests assume about the program. Tests are written from this file and the tickets' acceptance criteria; the implementation conforms to it. A change to the outside is made here first.

Each section says which ticket introduced it. Words in capitals are from `CONTEXT.md`.

## How the tests get the program

The harness runs `cargo build -p vm --bin vm` once and starts `target/debug/vm`, unless the environment variable `VM_BIN` names a binary to use instead. `blackbox` depends on no product crate.

## How the tests get a Postgres

The harness runs a private Postgres of its own, never through the program under test: from the binaries in the directory `VM_TEST_PG_BIN` names (`postgres`, `initdb`, `pg_ctl`), else from a Postgres found on the machine, else from prebuilt binaries it downloads once to a cache (`docs/research/local-postgres-without-docker.md`). No Docker, no administrator rights. It hands the program a superuser's connection address. Each test's program gets a platform database of its own name on that instance.

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

Kebab-case strings, the same everywhere a refusal is reported (to Application code, in the log, later in the Operation log and in the tool's output). Known so far: `no-request`, `unknown-operation`, `not-declared`, `not-registered`, `declared-lower`, `no-grant`, `log-unavailable`. Reserved for the ticket that introduces it: `not-own-database`.

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
| 413 | A body larger than 16 MiB (exactly 16 MiB is accepted) | `too-large` |
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
                  "read": { "role": "...", "password": "..." } } }
```

- A Resource's name is 1 to 63 characters: lower-case letters, digits and underscores, starting with a letter. Otherwise `400 invalid-name`. A taken name is `409 name-taken`. `kind` is `postgres`.
- `name`, `kind`, `description`, `owner`, `contact` and `connection` with its `read` login are required.
- Answers `201` with `{ "name", "kind", "description", "owner", "contact", "created_by", "created_at" }`.
- **No answer of the API, no log record and no error message ever holds a Resource's password**, and none holds its host, port, database or role either.
- Registering does not connect to the database (the ticket on Resources in full adds that check).

`GET /v1/resources/<name>` and `GET /v1/resources` (`{ "resources": [ ... ] }`, by name): the same object, for every signed-in User. A Resource's existence, description and whom to ask are visible to all; nothing else is.

`POST /v1/resources/<name>/grants`, by a Platform administrator, with `{ "group": "<Group>", "access": "structure" | "read" | "write" }`: sets that Group's Grant on the Resource, replacing the level if the Group already had one. Answers `200` with `{ "resource", "group", "access" }`. A Group is 1 to 256 bytes with no control characters, compared exactly.

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
- Malformed arguments (no `resource`, no `sql`, a param that is an object): `{ error: { kind: "malformed", message } }`, and nothing reaches any database.

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
- A refused or malformed operation sends nothing to the shared database.

## The Operation log (same ticket)

- Every `query` operation, allowed or refused, has one entry, written to the platform database **before** the statement is sent to the shared database and completed when it ends. If the entry cannot be written the operation is refused with `log-unavailable` and the statement is not sent.
- `GET /v1/operation-log`, by a Platform administrator: `{ "entries": [ ... ] }`, oldest first. `?limit=<n>` gives the newest `n` (1 to 1000, default 100), still oldest first.

```json
{ "id": 17, "time": "<RFC 3339>", "request": "<request id or null>", "user": "<user id or null>",
  "application": "incident-board", "version": 2, "resource": "devops", "operation": "query",
  "statement": "select id, title from incidents where open = $1",
  "outcome": "allowed" | "refused" | "failed" | "unfinished",
  "reason": "<refusal reason, when refused>", "error": "<SQLSTATE, or the limit, when failed>",
  "rows": 2, "duration_ms": 3 }
```

- `statement` is the statement **without its values**: every constant written into the SQL text is replaced by a placeholder, and parameters are never recorded. A statement that cannot be parsed is recorded as `"<unparsed>"`. No field of any entry holds a value from a statement, a parameter, or a row.
- `rows` and `duration_ms` are null unless the statement ran. `outcome` is `unfinished` while it runs, and stays so if the program died meanwhile.
- A malformed operation has no entry. `whoami` is not an operation on a Resource and has none.
- The program's own log still gets its `host-call` record for each call, as before.

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
