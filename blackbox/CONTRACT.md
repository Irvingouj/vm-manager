# The outside of the `vibe` program

What the black-box tests assume about the program. Tests are written from this file and the tickets' acceptance criteria; the implementation conforms to it. A change to the outside is made here first.

Each section says which ticket introduced it. Words in capitals are from `CONTEXT.md`.

## How the tests get the program

The harness runs `cargo build -p vibe --bin vibe` once and starts `target/debug/vibe`, unless the environment variable `VIBE_BIN` names a binary to use instead. `blackbox` depends on no product crate.

## How the tests get a Postgres

The harness runs a private Postgres of its own, never through the program under test: from the binaries in the directory `VIBE_TEST_PG_BIN` names (`postgres`, `initdb`, `pg_ctl`), else from a Postgres found on the machine, else from prebuilt binaries it downloads once to a cache (`docs/research/local-postgres-without-docker.md`). No Docker, no administrator rights. It is a Postgres 18, the one version the platform supports ("One Postgres version"): nothing chooses another. A `VIBE_TEST_PG_BIN` that names a Postgres that is not 18 stops the harness with words that say so; a Postgres found on the machine that is not 18 is passed over, and the 18 is downloaded. For the tests that need a server of another version, the harness downloads a Postgres 17 beside it. A download of the harness that does not bring the file (the connection fails or is cut, the time limit passes, or the answer's status is not 200) is tried again: four tries in all, two, four and eight seconds apart, each failed try said in the test's output with its address and what happened; after the fourth the harness stops with the last failure and the number of tries. What was fetched but is wrong is never tried again: an archive whose checksum is not the one pinned in the harness, or one larger than the limit, stops the harness at once. The harness asks the network for the archive alone: the checksum it holds the archive to is the pinned one, and the checksum file published beside the archive (`<archive>.sha256`) is not fetched, since it could only say the same or stop a test that had the right archive within reach. (The release run for `v0.1.0` and the rehearsal before it each lost their image job to a `404` from Maven Central for that checksum file, which was there minutes later and which the gate jobs of the same runs had been given.) It hands the program a superuser's connection address. Each test's program gets a platform database of its own name on that instance.

## Serving (ticket: Walking skeleton)

### Starting and stopping

```
VIBE_DATABASE_URL=postgres://<role>:<password>@<host>:<port>/<platform database> \
vibe serve --domain <domain> --listen <ip:port> [--stand-in-sign-in] [--administrators-group <group>]
```

- `--domain` is the installation's domain. `--listen 127.0.0.1:0` picks a free port.
- `--administrators-group` names the Group whose members are Platform administrators. Without it nobody is one. (From the ticket on the tool's sign-in through the browser on, it belongs to the stand-in; with OIDC the Group is the connection's.)
- `VIBE_DATABASE_URL`, in the environment and never on the command line, names the platform database and a role that may create databases and roles. See "The platform database".
- A start-up that cannot work exits non-zero, within 15 seconds, with a message on standard error naming the problem: no `VIBE_DATABASE_URL`, one that cannot be parsed, a Postgres that cannot be reached or refuses the role, a platform database written by a newer program. The message never shows the password.
- (Ticket: Walking skeleton named fixture Applications with `--application <name>=<path>`. That option is gone: Applications enter through the platform's API.)
- The program writes its log to standard error, one JSON object per line, each with `"event"` and `"time_ms"`.
- When it is ready to serve it writes `{"event":"listening","address":"127.0.0.1:<port>","stand_in_sign_in":<bool>, ...}`.
- It is stopped by ending the process. Nothing is promised about requests in flight at that moment.

### Addresses

- A request whose `Host` is `<application>.<domain>` goes to that Application's serving Version. Letter case and a port in `Host` are ignored.
- `api.<domain>` is the platform's API. The names `api`, `auth`, `dashboard`, `git`, `admin` and `www` are the platform's own and can never be an Application's.
- Tests connect to the listening address and set `Host`.

### Stand-in sign-in

A development-only way for a caller to state who the User is, for tests and the local run. Real sessions are in "Sign-in and sessions".

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

Kebab-case strings, the same everywhere a refusal is reported (to Application code, in the log, later in the Operation log and in the tool's output). Known so far: `no-request`, `unknown-operation`, `not-own-database`, `not-declared`, `not-registered`, `declared-lower`, `no-grant`, `log-unavailable`; and, from the ticket on a write whose answer is lost, `grants-unknown`.

## The platform database (ticket: Applications and their Versions live in the platform database)

- The program keeps everything it must remember in the platform database named by `VIBE_DATABASE_URL`. If that database does not exist the program creates it. It creates its tables at first start and brings an older platform database up to date at start, in place.
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

`GET /v1/applications/<name>` answers `200` with the same object. In this ticket every signed-in User may read it; from the ticket on Contributors and sharing on, only who can open it and Platform administrators ("Contributors and sharing", which also adds to the object).

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
- `GET /v1/applications/<name>/versions` answers `{ "versions": [ ... ] }`, oldest first. (Who may read Versions: "Contributors and sharing: On the platform's API".)
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
| `grants-unknown` | In place of the check of the Grant: the program cannot tell now which Grants are on the Resource ("A write whose answer is lost") | `resource` |
| `log-unavailable` | The Operation log entry could not be written | `resource` |

- `fix` is a sentence a Coding agent can act on. For `no-grant` it names the Resource's contact.
- A User holds the highest level any of their Groups is granted. `structure` < `read` < `write`.
- A Grant written or changed applies to every operation decided after it: from the next request at the latest, and also to the later operations of a request already in flight.
- The program decides from what it holds in memory (Applications, Versions, Resources, Grants, and from the ticket on Contributors and sharing each Application's two lists), which it keeps exactly in step with the platform database it alone writes. So while the platform database cannot be reached, Applications are still routed and operations are still decided; what cannot be logged is refused. What "in step" means when the answer of a write is lost: "A write whose answer is lost".
- A refused or malformed operation sends nothing to the shared database.

## The Operation log (same ticket)

- Every `query` operation, allowed or refused, has one entry, written to the platform database **before** the statement is sent to the shared database and completed when it ends. If the entry cannot be written the operation is refused with `log-unavailable` and the statement is not sent. An operation refused for another reason keeps that reason: when its entry cannot be written it is answered, once the program has stopped waiting for the entry, with its own refusal (`no-grant` stays `no-grant`), and the program's log has an `operation-log-failed` record.
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

- database `<p>_app_<n>`, whose comment is `vibe application <name>`;
- role `<p>_app_<n>_owner`: applies migrations and owns what they make (and, from the ticket "The Change history is kept out of a Contributor's reach" on, nothing of the platform's: see that section for the third role);
- role `<p>_app_<n>_runtime`: what Application code runs as. It owns nothing. It may read and change rows in the tables the migrations made (`SELECT`, `INSERT`, `UPDATE`, `DELETE`, and use of their sequences), in tables made by later migrations too, and nothing else: it cannot create, alter or drop anything, not even temporary tables; it cannot `TRUNCATE`; it is a member of no other role; it cannot connect to the platform database or to any other Application's own database, and no other Application's roles can connect to this one.
- The schema `vm` in an own database is the platform's: it may hold the platform's own tables and functions, which the runtime role cannot change. Everything the Application's migrations make is its own, by default in `public`.
- Their passwords are the platform's own and never appear in an answer, a log record or an error message.
- The platform needs only what `VIBE_DATABASE_URL`'s role was promised: creating databases and roles. It does not need a superuser.
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
- the answer is `{ error: { kind: "database", code: "VM004", message } }`, from `execute` or from `tx.commit` (through the SDK: `OperationFailed` of kind `database` with that code); the message says that an Application's structure changes in migrations only;
- the Operation log entry (of the `execute`, or of the `commit`) is `failed` with `error` `VM004`; the transaction is over, as after any commit.

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
  - The relay keeps at most 32 connections to own databases at once (their `application_name` is `vibe relay`), and an own database keeps one no longer than one batch while another waits for one. So the promise holds also when more than 32 own databases have large outboxes to ship at once: a write of yet another Application is shipped, and a read of its history answered, after a number of batches that does not grow with the size of the others' outboxes.
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

- **Exit codes.** `0`: done. `1`: failed (the installation cannot be reached, it answered something unexpected, a file cannot be read or written). `2`: the command line is wrong. `3`: refused, or waiting for the Builder (the platform said no, or the next step is a person's: the message says whose and what). `4`: not signed in (no credential, or one that has expired or was revoked): sign in and run the command again. `vibe --help` lists these five, each with its number and meaning.
- **`--json`**, on every command: standard output is exactly one JSON object and a line end, nothing else. A command that does not end with `0` prints `{ "error": { "reason": "<kebab-case>", "message": "<a sentence a Coding agent can act on>" } }` there (for exit code `2` the usual usage text on standard error is enough). Without `--json`, the result is text on standard output and an error is its message on standard error.
- Reasons the tool itself gives: `not-signed-in` (4), `sign-in-expired` (4), `sign-in-refused` (3), `unreachable` (1), `unexpected-answer` (1), `credential-file` (1: the credential file or its folder cannot be read, written or made, or the file does not hold what the tool wrote there; the message names the path). A refusal of the platform's API (an answer of `4xx` other than `401`) keeps the API's own `reason` and `message` and ends with `3`; an answer of `5xx` keeps them too and ends with `1`: the platform could not do it now.
- Every wait has an end: an installation that does not accept the connection within 5 seconds, or does not answer within 30, is `unreachable`. An answer whose body is larger than 8 MiB is `unexpected-answer`.
- **Without `--json`, nothing the installation answered reaches the terminal raw**: a control character in a name, a description or any other text of the installation's (also a line end, and the escape character that begins a terminal's control sequence) is printed as `\u{<hex>}`, so that what one User wrote cannot move the cursor, recolour or hide text on another's terminal. The same for the characters that reorder text without being control characters: the bidirectional marks and overrides U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. With `--json` each of these is written as a JSON escape (`\u001b`, `\u202e`), so the output holds none raw and still parses to the original text.
- A command whose standard output is closed before it has written (`vibe resources | head -0`) ends without a crash: with the exit code it would have had, or `1`, and nothing but an ordinary message, if anything, on standard error. A closed standard error is no crash either.
- The messages for `not-signed-in` and `sign-in-expired` say to run `vibe sign-in`, and `sign-in-expired` gives the whole command with the installation it was signed in to.

### Signing in

```
vibe sign-in --domain <domain> --connect <ip:port> --stand-in-user <id> [--stand-in-groups g1,g2] [--json]
vibe sign-out [--json]
vibe whoami [--json]
```

- `vibe sign-in` asks the installation for a **tool credential** for that User and keeps it on the Builder's machine. While sign-in is the stand-in, the User and their Groups are stated on the command line (`--stand-in-user`; from the ticket on the tool's sign-in through the browser on, the tool asks the installation which sign-in it has, and the stand-in without `--stand-in-user` is `sign-in-refused`, not exit code `2`); with OIDC the tool signs in through the browser ("The tool signs in through the browser, and OIDC is connected from the tool"), and nothing else here changes.
- With `--connect` the tool speaks plain HTTP and only to the address given, which must be a loopback address (`127.0.0.1:<port>`, `[::1]:<port>`); another address is exit code `2`. It sends `Host: api.<domain>`. Without `--connect` it speaks HTTPS to the installation ("The tool reaches an installation over HTTPS"); until that ticket `--connect` was required.
- It prints the User, their Groups and the installation: `{ "user": "<id>", "groups": [...], "platform_administrator": <bool>, "domain": "<domain>", "expires_at": "<RFC 3339 time>" }`. An installation that does not accept the sign-in (the stand-in is off) is `sign-in-refused`, exit code `3`, and nothing is kept.
- Signing in again replaces the credential kept; the earlier one is ended at the installation if it can be reached. A sign-in whose credential cannot be kept (the file cannot be written) is `credential-file`, and the credential just made is ended at the installation: none is left alive that no file holds.
- `vibe whoami` asks the installation who the credential is: `{ "user", "groups", "platform_administrator", "domain" }`. The Groups are those of the sign-in.
- `vibe sign-out` ends the credential at the installation and removes it from the machine: `{ "signed_out": true, "ended_at_installation": <bool> }`, exit code `0`. If the installation cannot be reached the credential is removed from the machine all the same (`ended_at_installation` false). A credential the installation says has ended already counts as ended there (`ended_at_installation` true). With nothing to sign out of: `{ "signed_out": false, ... }`, exit code `0`.

### The credential

- It is kept in the user's own configuration folder: `$XDG_CONFIG_HOME/vibe/` when that variable holds an absolute path, else `$HOME/.config/vibe/` (Linux and macOS; Windows comes with the ticket on the three systems). The tool makes the folder with mode `0700` if it is not there, and writes the file with mode `0600` whatever mode it had. After a sign-in the folder holds one file, a text file with the credential in it; its name and the rest of its form are not promised.
- **It is never printed**: not on standard output or standard error of any command, with or without `--json`, not in an error message, and it is not taken from the command line or the environment. It is not in the program's log on the installation's side, and the platform database does not hold it in a form that can be used (it keeps a digest).
- It is 32 random bytes or more, made by the installation, and cannot be guessed. Its text begins with `vmtc_`, so that one found where it should not be can be recognised.
- It ends when its time is up, when it is signed out, and when its User makes a seventeenth: a User holds at most 16 live credentials (one a machine), and making one more ends the one made longest ago. Never otherwise in this ticket. Its lifetime is 30 days from the sign-in; `vibe serve --tool-credential-lifetime <seconds>` sets another (tests use a short one).

### On the platform's API

- `POST /v1/tool-credentials`, by a signed-in User who signed in otherwise than with a tool credential (for now: the stand-in), with an empty body or `{}`: `201` with `{ "credential": "<the credential>", "user": "<id>", "groups": [...], "platform_administrator": <bool>, "expires_at": "<RFC 3339 time>" }`. This is the only answer that ever holds a credential. Asked with a tool credential it is `403` with reason `not-with-a-tool-credential`: a credential cannot renew itself.
- `Authorization: Bearer <credential>` on a request to `api.<domain>` is a sign-in like any other, as the User and the Groups of the moment the credential was made: every request of the API may be made with it. A request with an `Authorization` header is decided by it alone: a credential that is unknown, expired or signed out is the usual `401` (the same plain body as for anyone not signed in), whatever stand-in headers come with it; malformed stand-in headers are still `400` first.
- A tool credential is not a sign-in on an Application's address: there the header counts for nothing, and the request is `401` unless signed in otherwise. An `Authorization` header that holds a tool credential, or what looks like one (`Bearer vmtc_...`, the scheme in any letter case; by the prefix alone, alive or not, so nothing is looked up), is never handed to an Application's code: the request reaches it without that header, so a credential sent to the wrong address by mistake is not an Application's to use. Any other `Authorization` header is handed on as before.
- `DELETE /v1/tool-credentials/current`, with the credential: `204`, and the credential is ended at once. Signed in otherwise: `404` `not-found`. Any other method on the two paths is `405` `method-not-allowed`.
- `GET /v1/me`, by any signed-in User: `{ "user": "<id>", "groups": [...], "platform_administrator": <bool> }`.
- The `401` for a credential that has ended does not say whether it expired, was signed out or never existed. The tool calls all three `sign-in-expired`.

### Listing Resources

```
vibe resources [--json]
```

- `GET /v1/resources` and `GET /v1/resources/<name>` gain, in each Resource, `"your_access": "structure" | "read" | "write" | null`: the highest Access level the User holds on it through their Groups, null when they hold no Grant.
- `vibe resources --json` prints `{ "resources": [ { "name", "kind", "description", "owner", "contact", "your_access" } ] }`, by name: **every** registered Resource, also those the User holds no Grant on.
- Without `--json`, one block per Resource with the same facts in words. For a Resource with no Grant it says so and says whom to ask, with the Resource's `contact` (`no access: ask <contact>`); for one held at `structure` it says that rows cannot be read at that level.
- It shows no structure: no table, no column, nothing of what is inside a Resource, and nothing of its connection.
- With no Resource registered: `{ "resources": [] }`, and in words that none is registered and that a Platform administrator registers them.

### Listing Applications

```
vibe applications [--json]
```

- `GET /v1/applications`, by any signed-in User: `{ "applications": [ { "name", "description", "address", "contributor": <bool>, "serving_version": <number or null> } ] }`, by name: the Applications the User contributes to or can open. `address` is `<name>.<domain>`.
- **Until the ticket on Contributors and sharing says who can open an Application, the list holds the Applications the User is a Contributor of** (`contributor` true), and no other. That ticket adds the ones shared with them ("Contributors and sharing").
- `vibe applications --json` prints the same object. Without `--json`, one block per Application: name, description, address, and whether a Version is serving.
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

- Its first lines are `// Generated by vibe pull. Do not edit.` and `// Resource: <name>  Fingerprint: sha256:<64 hex>`.
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
vibe pull <resource> [--project <directory>] [--json]
```

- The project is `--project`, or the current directory. A directory without `manifest.ts` is not one: reason `not-a-project`, exit code `1`, nothing written.
- It asks the installation for the structure and writes two files: `resources/<name>/schema.ts` (the definition, as answered) and `resources/<name>/index.ts` (the generated link of "A project", with the `relations` lines exactly when `resources/<name>/relations.ts` is there). It makes the folders it needs. Each file is written whole or not at all.
- It prints `{ "resource", "fingerprint", "your_access", "files": [ { "path": "resources/<name>/schema.ts", "changed": <bool> }, ... ], "unsupported": [ ... ] }`, exit code `0`. `path` is relative to the project. Without `--json`, the same in words; each unsupported column is one line that names the table, the column and the type, and says the column is typed `unknown`.
- **Pulling twice changes nothing**: the second pull of an unchanged structure writes no file (each `changed` is false, the files' bytes and modification times are as they were).
- A file at one of the two paths that does not begin with its generated first line (`// Generated by vibe pull. Do not edit.` for `schema.ts`, `// Generated. Do not edit.` for `index.ts`) was not written by the tool: neither file is written; reason `not-generated`, exit code `1`, the message names the file.
- **Refused** (`no-grant`): exit code `3`, the API's reason and message (whom to ask; do not build against the Resource), and nothing is written or changed, also when an earlier pull's files are there.
- Not signed in, an installation that cannot be reached, and the other endings are as for every command.
- A Resource's name is checked by the tool before it is used in a path: what is not a Resource's name as the API takes one is exit code `2`.
- The tool writes inside the project only: when `resources` or `resources/<name>` is a symbolic link, or `schema.ts` or `index.ts` there is one, nothing is written anywhere; reason `not-a-project`, exit code `1`, the message names the link.

## A project: the template, creating one and checking one (ticket: The project template, and creating and checking a project)

What a Coding agent starts from (ADR 0006), and the one command that says whether a project will deploy.

### Creating a project

```
vibe new <name> [--directory <directory>] [--json]
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
vibe.dev.json               the test Users file: one User, `outsider`, in no Group, and no Grant
test-rows/README.md       what test rows are and how their files are named (since the ticket on the local loop in the browser)
package.json              private; dependencies @vm/sdk, drizzle-orm; devDependencies typescript, drizzle-kit, esbuild; exact versions
tsconfig.json             strict; "paths": { "@vm/own-structure": ["./src/db/schema.ts"] }; includes manifest.ts, src, resources
.gitignore                node_modules/, and what is in the tool's own folder .vibe/ but for .vibe/packages/ (git keeps the tarball)
.vibe/packages/vm-sdk-<version>.tgz   the SDK this program carries, which package.json names by this path
```

- The SDK is not fetched from anywhere: the program carries it and writes it into the project, and `package.json` depends on that file (`"@vm/sdk": "file:.vibe/packages/vm-sdk-<version>.tgz"`). So `npm install` needs the registry (or its cache) only for the other packages. Their versions are exact and are those of the repository's own lock file, which from this ticket on has `drizzle-kit` too: what a project installs is what the gate tested.
- **A new project installs and passes the check untouched**: `npm install`, then `vibe check`, ends with `0`, with no problem, no warning and no note.
- **A new project is an Application as it stands**: built with the SDK's tooling and sent as a Version, it answers `GET /` with `200`, `text/html`, and a page that has the User's id, written as text of the page (escaped for HTML).
- `migrations/` of a new project holds `.gitkeep` and nothing else, and is in step with the schema, which has no table.
- `vibe.dev.json` is `{ "about": "<what the file is for>", "users": [ { "id": "outsider", "displayName": "Outsider (no access)", "email": "outsider@example.com", "groups": [] } ], "grants": [] }`. The local loop's tickets say how it is used.

### The instruction file

`AGENTS.md` is written for the Coding agent, in plain words, at most 120 lines. It says what an Application is and how it reaches data (`ctx.user().db(name)`, `ctx.user().ownDb()`, `ctx.app().ownDb()`), that access belongs to people and a refusal for a missing Grant is not a bug to work around, and how to work: look for an existing Application first, find the data, keep the Application's own data in its own database, check, deploy. It names only commands this program has: every `vibe <command>` in its code spans and code blocks is one that `vibe --help` lists, `vibe pull` and `vibe check` among them, and each later ticket that adds a command adds it to the file.

It carries the three corrections of `docs/research/agent-facing-discovery.md`:

- **Data loss.** A migration that destroys data is never confirmed by the agent on its own: it tells the Builder exactly what would be lost and asks, **unless the Builder has already clearly said that this data may be lost**, in which case it goes on and says what was lost. The file has the words `unless the Builder has already clearly said that this data may be lost`.
- **The test User with no access.** The file says `vibe.dev.json` holds a test User, `outsider`, in no Group and so with no Grant, and tells the agent to check what that User gets before it calls the work done. The file names `vibe.dev.json` and `outsider` in one paragraph.
- **Who can see a new Application.** The agent is told to report who can see a new Application **from the tool's own output**, naming the people or Groups the tool named, and never from its own guess. The file has the words `from the tool's own output`.

And one rule the trial showed is needed: when `vibe pull` is refused for want of a Grant, the agent stops and tells the Builder which Resource, what for, and whom to ask; it does not invent the Resource's tables, mock its data or quietly drop the feature. This is one paragraph of the file, which names `vibe pull` and the Builder and has the words `refused`, `ask`, `invent` and `mock`.

### Checking a project

```
vibe check [--project <directory>] [--timeout <seconds>] [--json]
```

The project is `--project` or the current directory; one without `manifest.ts` is `not-a-project`, as for `vibe pull`. The check needs the tool signed in (it asks the installation which Resources are registered and what the Builder holds), Node 22 or later on the `PATH`, and the project's packages installed. It writes nothing in the project, not even for a while: no file or folder of the project is made, changed or removed, `migrations/` included. It makes four checks, each from what is on the machine now, and reports every problem it finds, not only the first:

| `check` | What must hold | `reason` when it does not |
|---|---|---|
| `manifest` | `manifest.ts` is a plain literal of the right shape (as the SDK's tooling reads it, without running it) | the tooling's own: `manifest-missing`, `manifest-not-literal`, `manifest-invalid`, with `file`, `line`, `column` |
| `resources` | every Resource the Manifest declares is registered on the installation | `resource-not-registered` |
| | and is pulled: `resources/<name>/schema.ts` and `resources/<name>/index.ts` are both there, each beginning with the first line `vibe pull` gives it | `resource-not-pulled` |
| `migrations` | `migrations/` is in step with `src/db/schema.ts`: generating migrations now would write no migration (no `.sql` file; the journal `drizzle-kit` keeps beside them is not one). `migrations/` is as `drizzle-kit generate --dialect postgresql --schema src/db/schema.ts --out migrations` leaves it, or holds no migration at all. A change `drizzle-kit` asks a question about and writes nothing for (a renamed column) is not in step either | `migrations-behind`; and `own-schema-failed` when `drizzle-kit` cannot load `src/db/schema.ts` (it is not there, it throws, it imports what is not there): the message has the tool's own words and says to correct the file; and `migrations-unreadable` when `drizzle-kit` cannot read `migrations/` itself (a journal or a snapshot that is not as it wrote them), or when the journal and the `.sql` files do not agree (a migration the journal names whose `.sql` file is not there, which `drizzle-kit` does not look at; a `.sql` file the journal does not name): the message has the tool's own words or names the file, and says that migrations are never edited by hand and to bring the folder back as it was last committed. The journal is read up to 4 MiB (a larger one is `migrations-unreadable`); of the files that do not agree the message names ten, and then says how many more |
| `types` | the project type-checks (`tsc --noEmit` with the project's own `tsconfig.json` and TypeScript) | `type-error`, one problem per error, with `file`, `line`, `column` and the compiler's message; at most 50, and then one last problem of the same check and reason, with `"more": <how many more>` and no `file`, `line` or `column`, whose message says how many more |

- A tool's own words in a message are its error, not its stack: no line of a stack trace, and no path of the check's own folder (a file of the copy of `migrations/` is named as the project's, `migrations/...`).
- **Every problem's message names the fix**: for `manifest-not-literal`, what was found and that the Manifest must be a plain literal because the platform reads it without running code; for `resource-not-registered`, that no such Resource is registered, the names that are, and to correct the name or ask a Platform administrator; for `resource-not-pulled`, the command `vibe pull <name>` (and, when a file is there that the tool did not write, which file, and that it is to be moved away first); for `migrations-behind`, that `src/db/schema.ts` changed since the last migration and how migrations are generated (the command `vibe generate`, of the section "Generating migrations"); for `type-error`, the file and line (an error the compiler gives with no place, such as a `tsconfig.json` that is not there, is a `type-error` whose `file`, `line` and `column` are null).
- **A warning does not fail the check.** When the Builder's own Access level on a declared Resource is below the declared one (`structure` or none against `read`; `read` against `write`), the check warns: reason `own-grant-below-declared`, with `resource`, `declared`, `held` (`"structure"`, `"read"`, or `null` for no Grant) and `contact`; the message says that the Builder, and every User whose Groups hold no Grant at that level, will be refused that Resource's operations by the Application, and whom to ask.
- **A note says what is worth knowing.** A Resource that is pulled (a folder `resources/<name>/` whose `schema.ts` begins with the first line `vibe pull` gives it) and not declared in the Manifest is a note: reason `pulled-not-declared`, with `resource`; the message says to declare it or remove the folder.
- With `--json`, standard output is one object: `{ "ok": <bool>, "problems": [ { "check", "reason", "message", ... } ], "warnings": [ { "reason", "message", ... } ], "notes": [ { "reason", "message", ... } ] }`, and when `ok` is false also `"error": { "reason": "check-failed", "message": "<how many problems, and the first>" }`, as every command that does not end with `0` has. Without `--json`, the problems, warnings and notes in words: on standard output when the check passes, on standard error when it fails. Text of the installation's and of the project's files is printed as the tool prints any text that is not its own.
- **Exit codes**: `0` when there is no problem (warnings and notes or not); `1` when there is a problem (`check-failed`); `4` not signed in; and as for every command otherwise. What is looked at first, in this order: the command line (`2`), the project (`not-a-project`), the sign-in (`4`), then `node` (`node-missing`), then the packages (`not-installed`). What the check itself needs and does not find is exit code `1` with its own reason and the fix: `node-missing` (no `node` on the `PATH`, or older than 22), `not-installed` (the project's packages are not installed: run `npm install`; decided by whether `@vm/sdk`, `typescript` and `drizzle-kit` resolve from the project as Node resolves them).
- **The check has an end**: 300 seconds in all, or `--timeout <seconds>` (a whole number from 1 to 3600; anything else is exit code `2`). Past it the check ends with exit code `1`, reason `check-timed-out`, and the message names the check it was in. What it starts it also stops, and what those started in their turn: each tool runs in a process group of its own, and nothing of the group is left running when the check ends, in time or not. (A process that leaves its group on purpose is not followed; the check runs the Builder's own project as the Builder.)
- **A check that is interrupted** (`SIGINT`, `SIGTERM` or `SIGHUP`, as a Coding agent's harness sends when it gives up on a command) ends the same way: it stops what it started, and ends with exit code `1`, reason `interrupted`.
- What the check needs to write, it writes in a folder of its own (mode `0700`) in the system's temporary folder (`TMPDIR`), the tools it runs included (that folder is their `TMPDIR`), and removes it in every ending: passed, failed, out of time, interrupted. This holds too for a project whose `tsconfig.json` has the compiler keep build information (`incremental`, `composite`): none is written in the project.
- The check runs the project's own tools (`tsc`, `drizzle-kit`, the SDK's tooling), each found as Node resolves its package from the project and run with the `node` on the `PATH` (`typescript`'s `tsc` and `drizzle-kit` by the `bin` of their `package.json`; the SDK's `tooling/main.js`), and so whatever `src/db/schema.ts` imports: as the Builder, on the Builder's machine, like any build. It does not run `src/index.ts` or the Manifest, and it reads no `drizzle.config.*`: the schema is `src/db/schema.ts` and the migrations are in `migrations/`, always.
- When the Manifest cannot be read, the `resources` check has nothing to go by and is left out; the other checks are still made.

## Generating migrations (ticket: Generate migrations, stopping before data is lost)

Nobody writes a migration by hand: the tool makes it from `src/db/schema.ts`, by running the project's `drizzle-kit` on the Builder's machine, and stops before one that destroys data.

### The command

```
vibe generate [--project <directory>] [--name <name>] [--allow-data-loss] [--empty] [--timeout <seconds>] [--json]
```

- The project, `node`, the packages, the end of the command (`--timeout`, 300 seconds without it, `check-timed-out` becoming `generate-timed-out`), an interruption, the folder of its own in `TMPDIR`, the process groups, and the way a tool's words are quoted are all as for `vibe check`. It needs no sign-in and asks the installation nothing. What is looked at first, in order: the command line (`2`), the project (`not-a-project`), `node` (`node-missing`), the packages (`not-installed`; only `drizzle-kit` is needed).
- It runs `drizzle-kit generate --dialect postgresql --schema src/db/schema.ts` against a copy of `migrations/` in its own folder, looks at what that wrote, and only then writes in the project. **Nothing is written in the project unless a migration is generated**: every other ending leaves every file and folder of the project as it was.
- **A migration generated** is three files in `migrations/`: the migration `<name>.sql`, its snapshot `meta/<number>_snapshot.json`, and the journal `meta/_journal.json`, each written whole (a temporary file beside it, then renamed), the journal last. **All three or none**: when one of them cannot be written (reason `project-file`, exit code `1`, the message names it), those written before it are removed again, and the project is as it was. `migrations/` or `migrations/meta/` that is a symbolic link is not written through: reason `not-a-project`, nothing written. It prints, exit code `0`:

  ```json
  { "generated": { "name": "0001_add_notes", "file": "migrations/0001_add_notes.sql", "statements": 2, "destroys": [] },
    "files": [ "migrations/0001_add_notes.sql", "migrations/meta/0001_snapshot.json", "migrations/meta/_journal.json" ] }
  ```

  `files` in the order of their paths. `generated` has these four fields and no other. Without `--json`, the same in words, with the three files by path and the SQL's statements, and saying to read them and to commit the three files.
- `--name <name>` gives the migration's name after its number (`0001_<name>`): 1 to 100 lower-case letters, digits and `_`; anything else is exit code `2`. Without it the name is `drizzle-kit`'s own.
- **Nothing to generate**: when `src/db/schema.ts` has not changed since the last migration, exit code `0`, `{ "generated": null, "files": [] }`, and in words that the migrations are in step with the schema. Nothing is written.
- After a migration is generated, `vibe check` finds `migrations/` in step, and generating again generates nothing.
- **The platform applies what it generates**: the migrations of `migrations/`, in the journal's order, each as `{ "name": "<its tag>", "sql": "<the file's text>" }`, are what a Version carries (section "Migrations travel with the Version").

### A migration that destroys data

A generated migration destroys data when one of its statements:

| The statement | What is lost | In `destroys` |
|---|---|---|
| drops a table | the table's rows | `{ "what": "table", "table": "<schema.table>" }` |
| drops a column | the column's values | `{ "what": "column", "table", "column" }` |
| changes a column's type to one that is not wider | values that do not fit the new type are cut or the migration fails | `{ "what": "type", "table", "column", "from": "<type>", "to": "<type>" }` |
| drops a schema | what was in it | `{ "what": "schema", "schema": "<name>" }` |

- `table` is always `<schema>.<table>` (`public.notes`). `from` is the type as the last snapshot has it and `to` as the statement has it (`numeric(10, 2)`, `varchar(20)`). A dropped schema whose tables are dropped with it is each table and then the schema, in the order of the statements. A statement that destroys nothing (such as the `DISABLE ROW LEVEL SECURITY` that `drizzle-kit` writes before a `DROP TABLE`) is in neither list.
- **Wider** is exactly: `smallint` to `integer` or `bigint`; `integer` to `bigint`; `real` to `double precision`; `varchar(n)` to `varchar(m)` with m ≥ n, to `varchar` without a length, or to `text`; `char(n)` to `text`; `numeric(p,s)` to `numeric(p2,s2)` with s2 ≥ s and p2 − s2 ≥ p − s, or to `numeric` without precision. Every other change of type counts as destroying: the tool does not guess. The old type is read from the last snapshot; a type it cannot find there counts as not wider, and `from` is then null.
- **It stops**: exit code `3`, reason `data-loss`, nothing written. Beside `reason` and `message` the error has `"destroys": [ ... ]` (every one, in the order of the statements) and `"statements": [ "<each destroying statement, as the migration has it, without the breakpoint line>" ]`, and nothing else. The message names each thing that would be lost, says that deploying an older Version later does not bring it back, and says: ask the Builder, telling them exactly what would be lost, unless the Builder has already clearly said that this data may be lost; then run the command again with `--allow-data-loss`.
- **With `--allow-data-loss`** the migration is generated as any other, exit code `0`, and `generated.destroys` lists what it destroys; in words it says what will be lost when the migration is applied.
- `--allow-data-loss` on a migration that destroys nothing changes nothing.
- The decision is made on the statements, read with the platform's own Postgres parser (the one that splits a migration), not on a pattern in the text: `DROP TABLE` inside a string or a comment is not a statement. A migration the parser cannot read stops the command with reason `migration-unparseable`, exit code `1`, nothing written.

### A change `drizzle-kit` asks about

When a table or a column disappears from `src/db/schema.ts` and another appears in the same place, `drizzle-kit` asks whether it was renamed, and without a terminal it writes nothing and still ends with `0`. The tool notices: exit code `1`, reason `rename-not-generated`, nothing written, and never a word that says a migration was generated. The message says that `drizzle-kit` cannot tell a rename from a removal and an addition, and gives the path that needs no question, one step at a time, each followed by `vibe generate`:

1. add the new column (or table) to `src/db/schema.ts`, keeping the old one: `vibe generate`;
2. copy the values: `vibe generate --empty --name <name>` writes an empty migration; put the one statement that copies the values in it (`UPDATE ... SET <new> = <old>`);
3. remove the old column (or table) from `src/db/schema.ts`: `vibe generate`, which stops for the data that the old one holds, as above.

### An empty migration

`vibe generate --empty [--name <name>]` writes a migration with no statement, for the Builder's own SQL (copying values, as above): the only migration ever edited by hand, and only until it is committed. It is generated only when the schema is in step with the migrations (otherwise reason `migrations-behind`, exit code `1`: generate the pending change first; so too when the pending change is one `drizzle-kit` asks about). Output as for a generated migration, with `"statements": 0`. What is written into it afterwards is not looked at by this command; the deploy looks at every migration it sends.

### What stops it before `drizzle-kit` is asked

As `vibe check` reports them, here as the command's ending (exit code `1`, nothing written): `own-schema-failed` and `migrations-unreadable`, with the same messages. From this ticket on, the fix both commands give for `migrations-unreadable` names `vibe generate` for the migrations the schema then needs (never `drizzle-kit` run by hand), and, for a `.sql` file the journal does not name, says to remove it if it was never committed (bringing the folder back does not remove a file git does not know).

### The instruction file and the check

From this ticket on, the template's `AGENTS.md` names `vibe generate` in its table of commands and in its rules (where it said how to run `drizzle-kit` by hand), and `vibe check`'s `migrations-behind` message gives the command `vibe generate`.

### What this does not close

- A removed enum label, a tightened constraint (`NOT NULL`, a check, a unique) and a dropped default make a migration that may fail on the rows that are there; they lose nothing and are not stopped.
- What a Builder writes into an empty migration is theirs; the deploy asks again about every migration that destroys data when a Version is already serving.

## Deploying (ticket: Deploy: a Version built on the Contributor's machine goes live)

One explicit command that never waits for an approval (ADR 0009, ADR 0016). The Version is built on the Contributor's machine and sent whole; the platform creates the Application at its first deploy, applies the migrations, and switches the address.

### On the platform's API

- `POST /v1/applications/<name>/versions` takes one more field, optional: `"structures": { "<resource>": "sha256:<64 hex>" }`, the fingerprint of each shared structure the Version was built against. Every key must be a Resource the Version declares and every value a fingerprint in that form; otherwise `400 malformed`. Left out or `null`, it is an empty object. The hex digits are lower-case. The Version's record gains `"structures"`, as sent, for ever.
- The messages of `migration-failed` and `migration-refused` say that a Version still serves only when one does: for an Application with no Version serving they say that nothing was applied and name no Version. `vibe deploy` gives the words of `migration-refused` as the API has them.
- Nothing else of the API changes: the tool creates the Application with `POST /v1/applications` when there is none of the Manifest's name, with the Manifest's `description`.

### The command

```
vibe deploy [--project <directory>] [--allow-data-loss] [--timeout <seconds>] [--json]
```

The project, `node`, the packages, the folder of its own in `TMPDIR`, the process groups, an interruption and the way a tool's words are quoted are as for `vibe check`. `--timeout` (300 seconds without it; `deploy-timed-out`, the message naming what it was doing) holds everything the command does on the machine; sending the Version has its own end, below. What is looked at, in this order, each ending the command when it does not hold:

1. The command line (`2`), the project (`not-a-project`), the sign-in (`4`).
2. **Git.** `git` on the `PATH` (`git-missing`, exit code `1`); the project is inside a Git repository with at least one commit (`not-a-repository`, exit code `1`; also a repository whose commits are not named by 40 hex digits, the SHA-256 object format, since the platform records a commit by them); and **nothing of the project's folder is uncommitted**: no file changed, staged, removed or untracked (ignored files do not count), else reason `uncommitted-changes`, exit code `1`, with `"files": [ "<path relative to the project>", ... ]` (each file by its own path, also those of a folder git does not know and those whose names have spaces or other letters, a file renamed by both its names, in the order of their paths; at most 50, and `"more": <n>` when there are more), and a message that says to commit them and deploy again. The Version's commit is the repository's `HEAD`.
3. `node` and the packages, as for `vibe check`.
4. **The Application.** A Manifest that cannot be read has no name to look for: this step is left out and the checks of step 6 end the deploy. When the installation has an Application of the Manifest's name and the User is not a Contributor of it: exit code `3`, reason `not-a-contributor`; the message names its Contributors and says to ask one of them, or to choose another `name` in `manifest.ts`. Nothing is built or sent. When it has none, the deploy will create it.
5. **Each declared Resource is pulled again**, as `vibe pull` does. A structure that has changed since it was pulled is written into `resources/<name>/` (the only files a deploy changes in the project). A Resource the User holds no Grant on cannot be pulled, and neither can one whose shared database does not answer (`structure-unavailable`): the pulled definition in the project is used as it is, with the fingerprint its second line gives, and the deploy warns (below). A declared Resource that is not registered, or not pulled, is left to the checks.
6. **The checks of `vibe check`**, on the project as it now is: `manifest`, `resources`, `migrations`, `types`. A problem ends the deploy exactly as it ends the check: exit code `1`, reason `check-failed`, the same report. So a changed structure fails the deploy only when the code no longer type-checks against it; the files stay updated, and the message of the failed deploy says which Resources' structure changed.
7. **Data loss.** When a Version is already serving and a migration this Version adds destroys data (as "Generating migrations" decides it, and besides: a `TRUNCATE` or a `DELETE`, at the top of a statement or in its `WITH`, which only a migration written by hand has; `{ "what": "rows", "table" }`): exit code `3`, reason `data-loss`, with `"migrations": [ { "migration": "<its name>", "destroys": [ ... ], "statements": [ ... ] } ]`, one item for each such migration in their order, unless `--allow-data-loss` is given. The message is the one of `vibe generate`, and says this is asked again at deploy because the rows are there now. With no Version serving, nothing is asked: there are no rows. A changed type is judged against the snapshot of the migration before it, each migration by its own. An added migration the parser cannot read ends the deploy, when a Version is serving, with reason `migration-unparseable`, exit code `1`, as in `vibe generate`. With `--allow-data-loss` the migrations are sent as they are; a `TRUNCATE` is then refused by the platform, which never truncates a table that has history (`migration-failed`, code `VM002`, below): only a `DELETE` removes rows.
8. The migrations the serving Version has must be the beginning of the project's (same names, same text): else reason `migrations-diverged`, exit code `1`, saying which migration differs and to bring `migrations/` in line with the repository's history (another Contributor's migrations may not be pulled yet).

Then the Application is bundled with the SDK's tooling (a project that does not build: reason `build-failed`, exit code `1`, with the tooling's `errors`), the Application is created when there is none, and the Version is sent: the bundle, the Declared access of the Manifest, the migrations of `migrations/` in the journal's order, the commit, and the fingerprints. A migration whose text is not UTF-8 ends the deploy with reason `migrations-unreadable`, exit code `1`. When the bundle and the migrations together are over 16 MiB the tool sends nothing and ends with reason `too-large`, exit code `1`.

### What it prints

Exit code `0`:

```json
{ "application": "incident-board", "address": "incident-board.<domain>", "version": 3,
  "created": false, "git_commit": "<40 hex>",
  "migrations_applied": [ "0002_add_notes" ],
  "structures": { "devops": "sha256:..." },
  "pulled": [ { "resource": "devops", "changed": true, "files": [ "resources/devops/schema.ts" ] } ],
  "visible_to": { "contributors": [ "<user id>", ... ] },
  "warnings": [ ... ], "notes": [ ... ] }
```

- `created` is true when this deploy created the Application. `migrations_applied` are those this Version added. `pulled` has every declared Resource, by name; `files` are the ones the deploy changed.
- **Who can see it**: `visible_to.contributors` are the Application's Contributors, by user id, as the installation answers them. (From the ticket on Contributors and sharing on, `visible_to` is the object of "Contributors and sharing: The tool", and the words name whom the Application is shared with.) In words: that the Application is live at its address as Version n, and that it is visible to its Contributors only, each by name, and to nobody else until it is shared. The words never say more than that (no Group, no "your team").
- When a structure changed: the words (and a note, reason `structure-updated`, with `resource` and `files`) say that the pulled definition was updated, that the Version was built against the new structure, and to commit those files.
- **Warnings and notes** are the check's (`own-grant-below-declared`, `pulled-not-declared`); the deploy goes on. For a declared Resource the User holds no Grant on, the warning also says its structure could not be pulled again and the definition in the project was used. For one whose shared database did not answer, the warning is reason `structure-not-pulled`, with `resource`, and says the same.

### What the installation may answer

- `migration-failed` (`422`): exit code `1`, the reason, and beside it `migration`, `code` and `database_message` as the API gave them; the message has the database's error and says that nothing was changed and that the Version that was serving, when there was one, goes on serving.
- `migration-refused` (`422`): exit code `1`, with `rule` and `migration`.
- `not-a-contributor` (`403`, when the User stopped being one meanwhile), `migrations-diverged` (`409`), `too-large` (`413`: the bundle and the migrations together are over 16 MiB): the API's reason and message, exit code `3` for the first and `1` for the others.
- **An Application this deploy created stays** when its Version is then not taken, whichever way the deploy ends after creating it (a refusal above, no answer): it has no Version and nothing is at its address. The error then has `"created": true` beside its reason, and its message says that the Application now exists with no Version. The next deploy has `created` false.
- Sending a Version waits up to 120 seconds for the answer (migrations run before it). Past that: exit code `1`, reason `unreachable`, and the message says the Version may or may not have gone live and to look with `vibe applications` before deploying again.

### From a new project

A project made by `vibe new`, with its packages installed and committed, deploys as it stands: `vibe deploy` creates the Application, and a signed-in User gets the template's page at `<name>.<domain>`.

### The instruction file

From this ticket on the template's `AGENTS.md` names `vibe deploy` in its table of commands and in "How to work" (in place of the sentence that there is no deploy command), says a deploy needs everything committed, that a destroying migration is asked about again at deploy, that a migration written by hand removes rows with `DELETE` and never with `TRUNCATE` (which the platform refuses), and that the agent tells the Builder who can see the Application from the deploy's own output.

### What this does not close

- The Application's `description` is the Manifest's at its first deploy; a later change of it is not sent.
- The confirmation for data loss is the tool's: the API takes a Version whose migrations destroy data from whoever is a Contributor.
- A structure that changes between the pull and the answer is not noticed until the next deploy.
- The data-loss question reads the statements of a migration, not what they run: SQL inside a `DO` block or a function is not looked at, and an `UPDATE` that overwrites values is not counted.

## Versions, going back, and logs (ticket: Versions: list them, go back to the previous one, read logs)

A Contributor sees an Application's Versions and which one serves, goes back to an earlier one with one command, and reads what the Application printed and the errors it raised, request by request.

### Going back, on the platform's API

`POST /v1/applications/<name>/go-back`, by a Contributor, with `{ "from": <number>, "to": <number, optional> }`. A Platform administrator who is not a Contributor is `not-a-contributor` here, as anyone else: reading logs is all this ticket gives them.

- `from` is the Version the caller saw serving; `to` is the Version to serve instead, `from - 1` when it is left out or `null`. Answers `200`:

  ```json
  { "application": "incident-board", "serving_version": 2, "was": 3,
    "migrations_not_undone": [ "0002_add_notes" ] }
  ```

- **At once.** Once the `200` has been received, every new request to the Application is served by Version `to`, with its bundle and its Declared access. Requests already in flight finish on the Version they started on. `GET /v1/applications/<name>` has `serving_version` `to`.
- **Migrations are not undone.** The Application's own database stays as the latest Version's migrations left it. `migrations_not_undone` names the migrations the Application's latest Version has and Version `to` has not, in their order: Version `to`'s code now runs against a database that has them.
- **Versions are not removed or renumbered.** The Version gone back from is still a Version; the next one sent is numbered after the latest, and its migrations must continue the latest Version's, as before (they are the ones applied).
- **Only backwards.** `to` must be a Version of the Application numbered below `from`. To serve a later Version again, send it again.
- Refusals, after the ones every request of the API has (`not-found` for no such Application, then `not-a-contributor`, then `malformed`):

  | Status | When | `reason` |
  |---|---|---|
  | 400 | `from` is missing or not a positive integer; `to` is given and is not a positive integer below `from` | `malformed` |
  | 409 | The Application's serving Version is not `from` (someone deployed or went back meanwhile; also when it has no Version); the message names the one that serves | `serving-changed` |
  | 409 | `from` serves and is Version 1: there is none before it | `no-previous-version` |

  A refused go-back changes nothing. Two go-backs with the same `from` sent at once: one answers `200`, the other `serving-changed`.
- The program's log gets one record: `event` `went-back`, with `application`, `user`, `from`, `to`.
- Any other method on the path is `405`.

### The latest Version, and the list of Versions

- The Application's object (`POST /v1/applications`, `GET /v1/applications/<name>`) gains `"latest_version": <number or null>`: the highest-numbered Version, null when there is none. After a go-back it is above `serving_version`.
- `GET /v1/applications/<name>/versions` takes `?limit=<n>` (1 to 1000; any other value, or the parameter given twice, is `400` `malformed`): the newest `n` Versions, still oldest first. Without it: the newest 1,000. An answer never holds more.

### Logs, on the platform's API

The platform keeps, for each request an Application was given, what its code printed and the errors it did not catch, and gives them to the Application's Contributors and to Platform administrators.

`GET /v1/applications/<name>/logs`, by a Contributor of the Application or a Platform administrator; anyone else is `403` `not-a-contributor` (after `not-found`). Answers `200`:

```json
{ "entries": [
    { "id": "<cursor>", "time": "<RFC 3339>", "request": "<the request's x-vm-request-id>",
      "version": 3, "user": "<user id>", "method": "GET", "path": "/notes", "status": 500,
      "printed": [ { "level": "log", "message": "loading notes", "loading": false } ],
      "uncaught": [ "<the error's message and stack>" ],
      "truncated": false, "dropped_before": 0 } ],
  "next": "<cursor>" }
```

- **One entry for each request the Application's code was given.** A request answered before that (the platform was busy, the body was too large or came too slowly, a method the platform does not hand on) has none. `version` is the Version that served it; `user` the signed-in User's id; `path` is without the query; `status` is what was answered. `printed` is what its code printed, in order, `level` the console method's name: the lines the script printed while it was loaded for this request first, with `"loading": true` (the program's log has those records with a null `request`, as before), then the lines of the request itself, with `"loading": false`. `uncaught` are its `uncaught` records' `error`, each the message and the stack as the program's log has it.
- **What code prints after its request ended** (work it left unawaited, if it runs at all) is in no entry: only the program's log has it. So every entry is a request's, and none of its fields is null.
- **Order.** Entries are oldest first, in the order the requests ended. `id` is a cursor: text, not promised to be a number.
  - Without `after`: the newest `limit` entries (`?limit=<n>`, 1 to 1000, default 100).
  - `?after=<cursor>`: the entries after that one, the oldest `limit` of them. `next` is the cursor of the last entry answered; with no entry answered it is `after` as given, or, without `after`, a cursor after which only entries made later come. So reading with `after=<next>` again and again gives every entry once: this is how logs are followed.
  - `?request=<id>`: only the entry of that request (none when there is none, or when it is another Application's). It goes with `after` and `limit`.
  - A `limit` outside 1 to 1000, an `after` that has not the form of a cursor, a parameter given twice: `400` `malformed`. (A cursor is opaque: one the caller made up may be taken, and then answers as a place in the log.) Unknown parameters are ignored.
- **When it can be read.** An entry can be read within 2 seconds of its response, when the platform database answers. It is written after the request and never in its way: no request waits for it, and a request is answered the same when it cannot be written.
- **Bounds.** Each has a stated end, and `truncated` is true in an entry any of them cut:
  - a `message` longer than 8,192 bytes is cut to that (at a character boundary); an `uncaught` error longer than 16,384 bytes likewise;
  - at most 200 `printed` records and 64 KiB of messages for one request (the record whose message would pass 64 KiB is not kept, nor any after it), and at most 10 `uncaught`; what comes after is not kept in the entry (the program's log still has every record).
  - A NUL character in a message or an error is kept as U+FFFD (the platform database cannot hold it); the program's log has it as it was.
  - The platform keeps the newest 5,000 entries of an Application; older ones are deleted as new ones are written, at the latest when 100 more have been: an Application never has more than 5,100.
  - When entries are made faster than the platform database takes them (more than 1,024 waiting), entries are dropped, not requests held: the next entry kept has in `dropped_before` how many were dropped before it.
- Logs hold what the Application chose to print. They are not the Operation log: a Platform administrator's record of every operation is that one, and nothing here replaces or feeds it.
- Entries of an Application stay when a Version is gone back from. Any other method on the path is `405`.
- The program's own log (standard error) is as before: every record, unchanged.

### The commands

```
vibe versions <application> [--limit <n>] [--json]
vibe go-back <application> [--to <number>] [--json]
vibe logs <application> [--request <id>] [--after <cursor>] [--limit <n>] [--follow] [--json]
```

All three need a sign-in (`4`) and name the Application on the command line; an Application the installation does not have is the API's `not-found`, exit code `3`.

**`vibe versions`** prints the Application's Versions, oldest first, exit code `0`:

```json
{ "application": "incident-board", "address": "incident-board.<domain>", "serving_version": 2, "latest_version": 2,
  "versions": [ { "number": 1, "git_commit": "<40 hex>", "deployed_at": "<RFC 3339>", "deployed_by": "<user id>", "serving": false },
                { "number": 2, "git_commit": "...", "deployed_at": "...", "deployed_by": "...", "serving": true } ] }
```

- `deployed_at` and `deployed_by` are the Version's `created_at` and `created_by`. `latest_version` is beside `serving_version`. It prints the newest 100 Versions, or the newest `--limit <n>` (1 to 1000; else exit code `2`); when there are older ones, in words it says how many are not shown and that `--limit` shows more. The serving Version has `serving` true when it is among those printed; `serving_version` names it always (null, and `versions` empty, when the Application has no Version).
- In words: one line for each Version with its number, its commit, when and by whom, and the serving one marked with the word `serving` (no other line has that word); with none, that the Application has no Version yet and that `vibe deploy` sends the first.

**`vibe go-back`** asks which Version serves, then goes back from it: to the one before it, or to `--to <number>`. Exit code `0`:

```json
{ "application": "incident-board", "address": "incident-board.<domain>",
  "serving_version": 2, "was": 3, "migrations_not_undone": [ "0002_add_notes" ] }
```

- In words: that Version 2 serves now at the address, in place of Version 3; **that migrations are not undone**, always, and when `migrations_not_undone` names some, which they are, that the own database keeps what they made, that Version 2's code now runs against it, and that the next deploy must still carry them in `migrations/` (a deployed migration is never removed).
- `--to` must be a positive integer (else exit code `2`). An Application with no Version, or whose serving Version is its first: reason `no-previous-version`, exit code `3`, and the message says there is nothing to go back to. `--to` that is not an earlier Version of the Application: reason `no-such-version`, exit code `3`, naming the Versions there are. The API's `serving-changed` (someone deployed or went back between the tool's two requests): exit code `3`, with the API's message; run the command again after looking with `vibe versions`. `not-a-contributor`: exit code `3`.
- It asks nobody: going back destroys nothing and is undone by deploying.

**`vibe logs`** prints the Application's log entries, oldest first, exit code `0`:

```json
{ "application": "incident-board", "entries": [ ... as the API gives them ... ], "next": "<cursor>" }
```

- `--request`, `--after` and `--limit` are the API's (`--limit` outside 1 to 1000 is exit code `2`; an `--after` the API does not take is its `malformed`, exit code `3`). Without `--after` it prints the newest; `--after <next>` of an earlier run prints what came since.
- In words, for each entry: a first line with the time (RFC 3339, UTC), the request's id, the User, the Version, the method, the path and the status; then each printed line with its level; then each uncaught error with its stack, indented. A cut entry says it was `cut`, and dropped entries are said where they are missing. With no entry: `no entries` (yet).
- Everything of an entry is text of the Application's or of a User's: in words it is escaped as any text of the installation's is (a control character never reaches the terminal raw; a line end inside a message begins a new, indented line).
- **`--follow`** prints the newest entries as without it, then goes on printing each new entry as it is made, until it is interrupted (`SIGINT`, `SIGTERM`, `SIGHUP`): exit code `0`. It asks the installation again about once a second. With `--json` it prints one JSON object on a line for each entry (the entry, with `"application"` beside its fields) and no enclosing object: the one command whose `--json` output is more than one object. When the installation stops answering it ends with `unreachable`, exit code `1`, after what it has printed; when the sign-in ends, with exit code `4`. With `--json` such an ending is one last line, the usual `{ "error": ... }` object.
- A closed standard output ends `--follow` without a crash: exit code `1`, reason `output-closed`, whether it was closed before the command started or while it ran (it is noticed when the next entry is printed).

### Deploying after going back

`vibe deploy` compares the project with the Application's **latest** Version (`latest_version`, read by itself: a deploy never reads the list of Versions), not the serving one: its migrations are the ones applied. So after a go-back, `migrations_applied` names only what the new Version adds to the latest Version's, the data-loss question is asked about those alone, and `migrations-diverged` is told against the latest Version's. (In "Deploying", "the serving Version" and "a Version is already serving" read so from this ticket on: the Application's latest Version, and an Application that has a Version.)

### The instruction file

From this ticket on the template's `AGENTS.md` names `vibe versions`, `vibe go-back` and `vibe logs` in its table of commands, and says: to debug, read `vibe logs <name>` (a response's `x-vm-request-id` is what `--request` takes; `--after` with the `next` of the last run reads what came since; `--follow` does not end by itself); that `vibe go-back` is the quick way out of a bad deploy and does not undo migrations, so `migrations/` keeps them.

### What this does not close

- An entry is made when its request ends: a request that never ends within the platform's limits has none until then, and what was printed by a request in flight when the program stops is only in the program's log.
- Who went back and when is in the program's log only; the API does not list it.
- Entries still waiting to be written when the process is ended are lost (at most the last 2 seconds' when the platform database answers); the program's log has their records.
- The queue of entries waiting is one for all Applications: when one Application makes entries faster than the platform database takes them, other Applications' entries are dropped with its own, each told by `dropped_before`. No request is slowed.
- Logs are kept by count (5,000 entries an Application), not by age; an installation cannot set another number yet.
- A Contributor reads everything the Application printed, whatever User's request printed it.

## The local loop: one request as a test User (ticket: Local loop: run the Application on the Builder's machine and send a request as a test User)

The same program in its local mode (ADR 0010, ADR 0015). One command builds the project, runs it on the Builder's machine beside a private Postgres with a stand-in for each pulled Resource, sends it one request as a test User of the project, prints the response, and stops everything it started. Nothing of it reaches the installation, and nothing of a real Resource but its structure is on the machine.

### What a pull writes, from this ticket on

- `GET /v1/resources/<name>/structure` gains `"standin_sql"`: the statements that make an empty stand-in of the structure, an array of texts, one a statement, each ending with `;`, and `"contact"`: the Resource's contact. `vibe pull` writes a third file, `resources/<name>/standin.sql`, under the same rules as the other two (written whole or not at all, not written over a file the tool did not write, no link followed, unchanged when the structure is; it is in `files`). Its first lines are `-- Generated by vibe pull. Do not edit.`, `-- Resource: <name>  Fingerprint: sha256:<64 hex>` and `-- Contact: <the contact, on one line>`; then an empty line and the statements of `standin_sql` in order, an empty line after each.
- **A deploy's pull** (step 5 of "Deploying") writes `schema.ts` and `standin.sql` when the structure has changed since it was pulled, and both are in that `pulled` entry's and that note's `files`; it writes nothing when the structure is the same (a `standin.sql` that is missing stays missing until `vibe pull`), and it never touches `vibe.dev.json`.
- **The stand-in's statements**, one a statement, in this order, the same text for the same structure:
  1. each schema but `public`; each enum, with its labels;
  2. each table, with its columns in order, each with its type, `NOT NULL`, identity (`serial` kinds as such), a generated column with its expression, and the table's primary key and unique constraints. A view, a materialized view and a partitioned table are each an ordinary table with the same columns. A column whose type is not Postgres's own nor an enum listed (a domain, a composite, an extension's type) is `text`;
  3. each default, as one `ALTER TABLE ... ALTER COLUMN ... SET DEFAULT ...`;
  4. each foreign key between listed tables, as one `ALTER TABLE ... ADD CONSTRAINT ...`.
  It holds no row, no privilege, no owner, no comment of the database's, and nothing of the Resource's connection.
- **The Builder's own Grant is copied into `vibe.dev.json`** by the same pull: the file gains (or keeps) a test User `{ "id": "builder", "displayName": "The Builder (your own access)", "email": "builder@example.com", "groups": [ "builder" ] }` and the Grant `{ "group": "builder", "resource": "<name>", "access": "<your access>" }`, replacing the one that Group had on that Resource. A User `builder` already in the file is kept as it is, with `builder` added to its `groups` when it is not among them; a new one is added after the others, and a new Grant after the others. Everything else in the file stays as it was (other Users, other Grants, keys the tool does not know, each key in its place; a number keeps its value, not its spelling), and the file is written whole, two spaces a level, only when something changed. The pull's output gains `"test_users": { "file": "vibe.dev.json", "changed": <bool>, "grant": { "group": "builder", "resource", "access" } }`.
- A `vibe.dev.json` that is not there is made, with `outsider` as a new project has it. One that cannot be read as the file described below ends the pull before anything is written: reason `test-users-file`, exit code `1`, the message says what is wrong in it.

### The test Users file

`vibe.dev.json`: `{ "about": <any>, "users": [ { "id", "displayName", "email", "groups": [ "<Group>", ... ] } ], "grants": [ { "group", "resource", "access": "structure" | "read" | "write" } ] }`.

- `users` is required; `grants` left out is no Grant; `about` is anything or left out. A User's `id` is 1 to 256 bytes with no control character, unique in the file; `groups` is required and may be empty; `displayName` and `email` are optional text. A Grant's `group` is a Group as the API takes one, `resource` a Resource's name, and one Group has at most one Grant on one Resource. Keys the tool does not know are ignored. Anything else is reason `test-users-file`, exit code `1`, naming what is wrong and where.
- The Grants of the file are the Grants of the local run, all of them and no other: one removed from the file is gone at the next run. A Grant on a Resource that is not pulled is a note (`grant-without-resource`) and does nothing.

### The command

```
vibe request <path> --as <test user> [--method <METHOD>] [--header "<name>: <value>"]... [--data <text> | --data-file <file>]
           [--project <directory>] [--timeout <seconds>] [--json]
```

- `<path>` begins with `/` and may have a query. `--method` is `GET` without it, `POST` when `--data` or `--data-file` is given; any token is taken by the tool. One the platform does not hand to an Application (anything but `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE` and `OPTIONS`) gets the platform's own `405`, as deployed: that is the response printed (exit code `0`), with nothing printed by the code and no operation. `--header` may be given several times; `Host`, `Content-Length` and every header whose name begins with `x-vm-` (in any letter case) are the tool's and are refused on the command line (exit code `2`), as is a `<path>` that does not begin with `/`, a header without `:`, both `--data` and `--data-file`, and a body over 1 MiB.
- The project, `node`, the packages, the end of the command (`--timeout`, 300 seconds without it: `request-timed-out`, the message naming what it was doing), an interruption, the folder of its own in `TMPDIR`, the process groups and the way text is quoted are as for `vibe check`. **It needs no sign-in and asks the installation nothing**: it works signed out and with no installation reachable.
- What is looked at first, in order: the command line (`2`), the project (`not-a-project`), `vibe.dev.json` (`test-users-file`), `--as` (a User that is not in the file: reason `no-such-test-user`, exit code `1`, naming those that are), `node` and the packages (`node-missing`, `not-installed`).
- **Exit code `0` whenever the Application was given the request and a response came back, whatever its status**: a `500` is a result, not a failure of the command.
- Three more ways it ends with exit code `1`, each message naming the fix: a `--data-file` that cannot be read is reason `data-file`; a `.vibe/local/` that cannot be used (it is not a folder, or what it holds cannot be read) is `local-state`, and the message says that removing it starts the local databases anew; a private Postgres that does not start or does not answer is `postgres-failed`, and the message names `.vibe/local/postgres.log`, which has its own words. Whichever it is, nothing the run started is left running.

### What a run does

1. **A private Postgres**, version 18 (the one downloaded is 18.6.0: `io/zonky/test/postgres/embedded-postgres-binaries-<platform>/18.6.0/embedded-postgres-binaries-<platform>-18.6.0.jar` under the base address). Its programs are looked for in the user's own folder: `$XDG_DATA_HOME/vibe/postgres/` when that variable holds an absolute path, else `$HOME/.local/share/vibe/postgres/`. When they are not there they are downloaded once, into that folder (made with mode `0700`): the build for this machine from `https://repo1.maven.org/maven2` (the archive `io.zonky.test.postgres:embedded-postgres-binaries-<platform>`), checked against the SHA-256 the program carries for it before anything of it is unpacked or run, and unpacked beside its final place and renamed, so that an interrupted download leaves nothing that looks complete. The words of that run (not the JSON) say that Postgres is being downloaded, once, and where to.
   - `VIBE_LOCAL_POSTGRES_MIRROR=<base URL>` is used in place of `https://repo1.maven.org/maven2` (a company's mirror); the checksum is the program's all the same, so a mirror cannot hand over another archive. A download that fails, is cut short, is larger than 256 MiB or has another checksum: reason `postgres-unavailable`, exit code `1`, nothing left in the folder but what was complete before, and the message says what failed and names the two variables. What a download that was killed outright left there (a part of an archive, a folder half unpacked) is removed by the next download once it is 15 minutes old; a younger one may be another command's, at work, and is left.
   - `VIBE_LOCAL_POSTGRES_BIN=<directory>` names a Postgres 18 the Builder already has (`postgres`, `initdb`, `pg_ctl`): it is used, and nothing is downloaded. The version is the number after `(PostgreSQL)` in what `postgres --version` says, whatever follows it (a distribution's note in brackets, `beta1`). A directory without them, or of another version: `postgres-unavailable`.
   - A machine the program has no build for: `postgres-unavailable`, naming `VIBE_LOCAL_POSTGRES_BIN`.
   - Nothing needs Docker, administrator rights, or anything installed by hand; nothing is written outside the user's own folder, the project and `TMPDIR`.
2. **The project's local state** is the folder `.vibe/local/` of the project (git ignores it): the data of that Postgres, made at the first run (mode `0700`), and what the run needs to find it again. The Postgres listens on a loopback address only, on a port free at that moment, with a password of its own made at the first run; it is started for the run and **stopped before the command ends, however it ends** (also interrupted or timed out): no process the run started is left. A Postgres left running by a run that was killed outright is stopped and started anew by the next run. It is stopped only once it has shown it is this project's (it takes this project's own password on the port its `postmaster.pid` names, and says its data folder is this one): **no process is ever signalled on the word of a file alone.** A `postmaster.pid` left by a Postgres that is gone (the machine was switched off) is not believed, whatever process has its number now: that process is left alone, the file is removed, and the run goes on.
   - The folder also holds `vibe.log` (the program's own log of the run, which a local run does not write to the terminal) and `postgres.log`: each is started anew by every run, so neither grows from run to run.
3. **One local run a project at a time.** A second command of this section in the same project waits for the first, up to its own `--timeout`; then `request-timed-out`, saying that another local command of this project is running.
4. **A stand-in for each pulled Resource** (each `resources/<name>/standin.sql` the tool wrote): a database of its own in the private Postgres, made by running the statements of `standin.sql`, each on its own, as a role that owns that database and nothing else and is not a superuser (a statement that would read or write a file, run a program or reach another database fails like any other that fails). It has two logins: one that may only `SELECT` from every table, one that may also `INSERT`, `UPDATE` and `DELETE` (and use the sequences). It holds no row. It is made again from nothing when the file's fingerprint is not the one it was made from. A statement that fails (a default or a generated column whose expression needs something the local Postgres does not have) is left out, and the run warns: `{ "reason": "stand-in-incomplete", "message", "resource", "statement": "<the statement's first line>", "error": "<the database's message>" }`. A pulled Resource without `standin.sql` (pulled before this ticket) has no stand-in: a warning, reason `stand-in-not-pulled`, that says to run `vibe pull <name>` again. The stand-in of a Resource that is no longer pulled (its folder was removed) is removed by the next run, its database and its logins.
5. **The Resources and Grants of the run** are the stand-ins, each registered under its Resource's name with the contact of its `standin.sql`, and the Grants of `vibe.dev.json`. A Resource the Manifest declares that has no stand-in is not registered: operations on it are refused `not-registered`, as in an installation.
6. **The Application** is the project as it is now, built with the SDK's tooling as `vibe deploy` builds it (a Manifest the tooling cannot read: reason `check-failed`, with that problem as `vibe check` reports it; a project that does not bundle: `build-failed`). Uncommitted changes are fine, and no check of types or migrations is made: this is the quick loop. It becomes the local Application's serving Version: a new local Version whenever the bundle, the Declared access or the migrations differ from the local one before, numbered as an installation numbers them. A project whose Version would be larger than an installation takes ends as a deploy ends before it sends (`too-large`, exit code `1`), in the local run's own words. The local Application keeps its newest 50 Versions and forgets the earlier ones (their numbers are not used again), so the local state does not grow with every change of the code.
7. **The own database** is a local one, made at the first run, with the three roles and the capture of an installation. The migrations of `migrations/` that it has not yet are applied by the same rules as at a deploy, by the same code: so **a migration generated since the last run is applied by the next**, and one that fails or breaks a rule ends the command as it ends a deploy (`migration-failed`, `migration-refused`, exit code `1`, nothing changed). The data-loss question is not asked: a local database holds test rows only. When `migrations/` no longer begins with what the local own database has (a migration not yet deployed was generated again): reason `local-migrations-diverged`, exit code `1`; the message names the migration and says that removing the folder `.vibe/local/` starts the local databases anew.
8. **The request** is given to the Application as that test User: signed in as the User's `id` with the User's `groups`, at the address `<application>.localhost`. The same Operation gateway, Authorization decision and Database executor decide and run its operations as in an installation, against the stand-ins and the local own database; the Operation log and the Change history are written locally, in the private Postgres, and nowhere else.
9. **What the request recorded is waited for** before the command ends: its operations' entries, its log entry, its changes in the local Change history, each for at most 10 seconds. A part that did not come in that time is a warning, reason `records-incomplete`, naming the part; the response is printed all the same.

### What it prints

```json
{ "as": "builder", "method": "GET", "path": "/incidents",
  "application": "incident-board", "version": 3, "request_id": "<the response's x-vm-request-id>",
  "status": 200, "headers": { "content-type": [ "application/json" ] },
  "body": "<the body as text>", "body_base64": null, "body_truncated": false,
  "printed": [ { "level": "log", "message": "...", "loading": false } ], "uncaught": [],
  "operations": [ { ... the request's Operation log entries, as `GET /v1/operation-log` gives them ... } ],
  "migrations_applied": [ "0001_add_notes" ],
  "standins": [ { "resource": "devops", "made": true } ],
  "warnings": [ ... ], "notes": [ ... ] }
```

- `path` is `<path>` as it was given, with its query. `headers` has each response header's name in lower case with its values in order, as the platform sends them: a name the code appended twice is one value, the two joined with `, ` (as `fetch` has it), and each `set-cookie` is a value of its own. `body` is the body when it is UTF-8 text, else null and `body_base64` has it; the response to a `HEAD` has no body, as on the wire, whatever the code answered; at most 1 MiB of the body is printed (`body_truncated` true when it was longer).
- `printed` and `uncaught` are what the request's log entry holds ("Logs"); `operations` are the entries of the local Operation log whose `request` is this one, oldest first: so a refusal's `reason` and a statement's outcome are in the command's own output. `migrations_applied` are those this run applied; `standins` has every pulled Resource, `made` true when this run made or remade its stand-in.
- In words: the status and the headers, an empty line, the body; then what was printed, the uncaught errors, and one line for each operation (its Resource or the own database, its outcome, the reason of a refusal); then the warnings and notes. Everything of the Application's and of the project's is escaped as any text is (a control character never reaches the terminal raw); the body's line ends are kept.
- **A refusal is the same as deployed.** A request as a test User whose Groups hold no Grant on a Resource gets from `ctx.user().db(<name>)` the refusal an installation gives (`no-grant`, with `resource`, `user`, `needed`, `held` and the Resource's `contact`); one whose Grant is `structure` gets the `no-grant` of a User who may not read rows (`held` `structure`). The Application's own handling of it is what the response shows.

### Reading what was recorded locally

```
vibe local operation-log [--limit <n>] [--project <directory>] [--timeout <seconds>] [--json]
vibe local change-history [--table <[schema.]table>] [--limit <n>] [--project <directory>] [--timeout <seconds>] [--json]
```

- Both start the project's private Postgres as a run does, read, and stop it. They read everything the project's local runs recorded: a project whose Manifest's name was changed keeps what the earlier name's local Application recorded, until `.vibe/local/` is removed. A project that has had no local run: `{ "entries": [] }` and `{ "changes": [] }`, and nothing is made or downloaded.
- `vibe local operation-log --json` prints `{ "entries": [ ... ] }`: the newest `limit` entries (1 to 1000, 100 without it), oldest first, each as `GET /v1/operation-log` gives it.
- `vibe local change-history --json` prints `{ "changes": [ ... ] }`, newest first, at most `limit` (1 to 1000, 100 without it), of the table `--table` names (`public` without a schema) or of every table: each change with `id` (text), `at`, `schema`, `table`, `kind`, `key`, `old`, `new`, `changed`, `user_id`, `actor`, `request`, `migration` and `structure`, as the Change history records them (`old` and `new` are the rows as they were recorded, by column name). Every change committed by an earlier run is there: a run does not end before the changes of its request are in the local Change history.
- In words, one block an entry or a change. Exit code `2` for a `--limit` outside 1 to 1000.

### The instruction file

From this ticket on the template's `AGENTS.md` names `vibe request` in its table of commands and says: try the Application with `vibe request <path> --as <user>` before deploying, as `builder` and as `outsider`; that the Resources there are empty stand-ins with the real structure, so a list is empty and a refusal is real; that the Grants of `vibe.dev.json` decide, and `vibe pull` copies the Builder's own there; that the output has what the code printed and each operation's outcome; and `vibe local operation-log` and `vibe local change-history`.

### What this does not close

- Test rows in the stand-ins, opening the local Application in a browser, a command that resets the local databases, and working with a pull that cannot reach the installation are the next ticket's.
- Windows: the ticket on the three systems.
- A stand-in gives its read login every table and its write login every table: an installation's logins may be allowed less, table by table, and a view's stand-in can be written to where the view cannot.
- The archive of Postgres is a third party's build, pinned by its checksum; for the first release it stays that build, said in the release's notes (the ticket "A first, minimal release").
- Each run starts and stops its Postgres and builds the project: a request takes seconds, not milliseconds. The next ticket keeps the local Application running.
- `vibe.dev.json` is in the repository: the Grant a pull copies there is the one of whoever pulled last.

## The local loop, kept running: the browser, test rows and reset (ticket: Local loop in the browser, with test rows, reset and working offline)

`vibe dev` keeps the project's local Application running, so that the Builder opens it in a browser and chooses which test User they are; the stand-ins and the local own database get test rows from files of the project; one command starts the local databases over. Everything here is the local run of "The local loop: one request as a test User", kept up: the same private Postgres, stand-ins, Grants, Version and records, and nothing of it reaches the installation.

### The command

```
vibe dev [--port <n>] [--project <directory>] [--json]
```

- It does what a run of `vibe request` does up to the request (the private Postgres, the stand-ins, the Grants of `vibe.dev.json`, the local Version, the migrations, the test rows), then **listens and serves until it is interrupted**. Failures before it listens are those of `vibe request`, with the same reasons and exit codes. `SIGINT`, `SIGTERM` and `SIGHUP` end it: everything it started is stopped, as a run stops it, and the exit code is `0`.
- It listens on the loopback address only: `127.0.0.1`, and `::1` where the machine has it, on `--port` (1 to 65535; `7480` without it). A port that is taken: reason `port-in-use`, exit code `1`, naming `--port`.
- The address is `http://<application>.localhost:<port>/`. Browsers resolve `*.localhost` to the loopback address by themselves; a tool that does not connects to `127.0.0.1:<port>` and names the Application in `Host`.
- When it listens it says so: in words, on standard output, the address and that it runs until interrupted; with `--json`, one JSON object a line, the first `{ "event": "listening", "url": "http://<application>.localhost:<port>/", "application", "port", "version" }`. Then one line for each request the Application's code was given: in words the method, the path, the status and the test User; with `--json` `{ "event": "request", "as", "method", "path", "status", "request_id" }`. What the local run answers by itself (a `307`, a `421`, a `404`, anything under `/_vm/`, a request with no chosen User) has no line. A rebuild (below) is a line too: `{ "event": "rebuilt", "application", "url", "version", "migrations_applied", "standins", "test_rows" }`, or `{ "event": "rebuild-failed", "error": { "reason", "message", ... } }`. An output that is closed ends it as `vibe logs --follow` ends.
- **One `vibe dev` a project.** A second one in the same project: reason `already-running`, exit code `1`, at once, the message giving the address of the first. One started while the first has not yet said that it listens ends so as soon as the first does: it does not wait for the first to end.

### Which requests it serves

- **Only its own name.** A request whose `Host` is `<application>.localhost` (with or without the port) is the Application's. `localhost` and `127.0.0.1` (and `[::1]`), with or without the port, are answered `307` to the same path at `<application>.localhost:<port>`. Any other `Host` is answered `421` with a page of the local run's own that gives the address, and the Application's code is not run: a page of another site that points a name of its own at the loopback address reaches nothing.
- **Paths under `/_vm/` are the local run's own** and never reach the Application's code. Only `/_vm/test-user` is a page; `/_vm/control/` is what the tool's own commands talk to (below); anything else there is `404`.
- **Every answer of the local run's own** (its pages, its redirects, its refusals) carries `cache-control: no-store`, `x-content-type-options: nosniff` and a `content-security-policy` with `default-src 'none'`, `frame-ancestors 'none'`, `base-uri 'none'` and `form-action 'self'`: a page of the local run runs no script, loads nothing, and is shown inside no other page.
- Every other path is the Application's, served as an installation serves it (the same limits, the same headers taken away and added, `x-vm-request-id`), as the chosen test User.

### Choosing the test User

- `GET /_vm/test-user` is a page (`200`, `text/html; charset=utf-8`, `cache-control: no-store`) listing every test User of `vibe.dev.json` as it is now: the `id`, the `displayName` and `email` when there are, the Groups, and for each pulled Resource the access those Groups' Grants give (`write`, `read`, `structure`, or `none`), with one button for each User, and which one is chosen now. Everything of the file's is escaped as HTML.
- `POST /_vm/test-user` with a form body `user=<id>` and optionally `next=<path>` chooses: `303` to `next` (when it begins with one `/` and not with `//` or `/\`; else to `/`), with `Set-Cookie: vm_test_user=<the id, percent-encoded>; Path=/; HttpOnly; SameSite=Lax`. A `user` that is not in the file: `400`, the page again saying so. A `POST` with an `Origin` that is not this address (another site's, or `null`) is `403`; one without `Origin` is taken.
- Every `Location` the local run sends but the `307` is a path, not an absolute address. Where it percent-encodes (the cookie's value, `next`), every byte is encoded but letters, digits, `-`, `.`, `_` and `~` (and `/` in `next`).
- **A request with no chosen User** (no `vm_test_user` cookie, or one naming a User that is not in the file now) does not reach the Application: a `GET` or `HEAD` whose `Accept` has `text/html` is answered `303` to `/_vm/test-user?next=<the path and query, percent-encoded>`; any other is answered `401`, `text/plain`, saying to choose a test User at that address (or to send `Cookie: vm_test_user=<id>`).
- The Application is given the request as that User: the User's `id` and `groups` as the file has them at that moment. The cookie `vm_test_user` is taken out of the `Cookie` header before the request reaches the code (other cookies stay, byte for byte, also one whose value holds bytes that are not ASCII; a `Cookie` header left empty is left out). The chosen User is found whatever the other cookies hold.
- **This page is the local run's only.** `vibe serve` has no such page, with the stand-in sign-in or without: `/_vm/test-user` on a deployed Application is a path like any other and is given to its code.

### The project as it is now

- Before each request it serves (the browser's, or one a command sends), `vibe dev` looks whether the project changed since it last built: any file of the project but those under `node_modules/`, `.vibe/` and `.git/`, by its size and its time of modification (at most 20,000 entries are looked at; a project with more is rebuilt only when it is started again, and `vibe dev` says so once, when it finds it (as it starts, or at the request that found the project grown past them): `{ "event": "too-many-files", "message" }`, or the sentence in words). When it changed, it does again what a run does: the stand-ins of the pulled Resources (remade when their structure changed), the Grants of `vibe.dev.json`, the build, a new local Version when the bundle, the Declared access or the migrations differ, the new migrations, the test rows that changed. Requests wait for it, and those under way are finished first.
- **A change of the Application's name** in `manifest.ts` is a change like any other, and the address changes with it: the `rebuilt` line gives the new `application` and `url`, the request that found the change is answered by the `Host` rule as it is after the rebuild (`421` at the old name, which serves nothing any more), and the tool's own commands go on finding `vibe dev`. The local Application of the old name keeps what it had until a reset.
- **A project that does not build** (or whose `vibe.dev.json` cannot be read, whose migration fails, whose test rows fail): the request that found it is answered `500` with a page of the local run's own, `text/html`, giving the reason and the message that `vibe request` would give, escaped. The Version built before is not served meanwhile: every request is answered so until the project is corrected, and the first request after a change tries again.

### The tool's own commands while it runs

- While `vibe dev` runs in a project, `vibe request`, `vibe local operation-log`, `vibe local change-history` and `vibe local reset` in that project do not wait for it and start nothing: **they are answered by it**, with the same output, the same reasons and the same exit codes as alone. A `vibe request` is served like a browser's request (after the same look at the project), as the User `--as` names, and takes milliseconds, not seconds. Its `migrations_applied` and `test_rows` are what the rebuild it caused did (empty when it caused none); `standins` has every pulled Resource as always, `made` true for those that rebuild made.
- **A command started while `vibe dev` is starting** (it holds the project's lock and does not listen yet) waits as it waits for any other local command, and is answered by `vibe dev` as soon as that listens: it does not wait for `vibe dev` to end.
- **A `vibe request` is served alone**: it waits for the requests under way to end, and those that come meanwhile wait for it, so that the operations, the printed lines and the changes it reports are its own. Whatever `vibe request` takes alone it takes here: a body of 1 MiB with as many headers as a command line holds is answered the same.
- **The Application's code is given the same time in every local run**, alone or in `vibe dev`: the time an installation gives a request (30 seconds, until the ticket Limits per request says otherwise). A handler still running after that time is answered as an installation answers it (exit code `0`: a response came back); `--timeout` holds the whole command, and one shorter than that ends the command first (`request-timed-out`).
- An answer of `vibe dev` that is not the command's (it is stopping, or busy for longer than the command may wait) ends the command with exit code `1`, reason `failed`, the message saying that `vibe dev` runs in this project and could not do it now.
- They find it through a file of `.vibe/local/` (mode `0600`) that holds its port and a secret made for that run from the system's randomness. The secret is 64 lower-case hex digits. `/_vm/control/` answers `404` to a request without that secret, whatever it asks, and the secret is in no output, no log and no page. A file left by a `vibe dev` that was killed outright is not believed: the command sees that nothing holds the project's lock and runs alone.
- **`vibe.log` and `postgres.log` do not grow without end while `vibe dev` runs**: each is started anew once it is larger than 16 MiB.

### Test rows

- **Files of the project**: `test-rows/<resource>.sql` for the stand-in of a pulled Resource, and `test-rows/own-database.sql` for the own database. Each is SQL the Builder or the Coding agent writes: `INSERT` statements for the most part. A new project has the folder with a `README.md` saying this; a file for a Resource that is not pulled is a note (`test-rows-without-resource`) and does nothing. A file is at most 16 MiB (`test-rows-failed` past it). A `test-rows/<name>.sql` that is not a file (a link, a folder) is `test-rows-failed` too, with `file`, saying that test rows are files of the project: a link is not followed. `test-rows/own-database.sql` in a project that has no migration (its own database holds no table of the Application's yet) is a note (`test-rows-without-own-database`, with `file`, naming `vibe generate`) and does nothing.
- **A stand-in's rows.** The file's statements are run in order in one transaction by the stand-in's write login (so they can insert into, update and delete from every table, a view's and a materialized view's stand-in included, and can do nothing else: no table is made or changed). Before, every table of the stand-in is emptied and its sequences and identities start again, so loading the same file twice gives the same rows. It is loaded when the stand-in is made or remade, when the file's content is not the one loaded last (a file removed: the stand-in is emptied), and by a reset; **not otherwise**, so that what the Application wrote into a stand-in through a `write` Grant stays from one run to the next.
- **The own database's rows.** `test-rows/own-database.sql` is run in order in one transaction after the migrations, by the role the Application's own statements run as, so only what the Application itself could do. Each row it changes is recorded in the local Change history like any other change, with `user_id` `test-rows`, `actor` `application` and a `request` that begins with `test-rows-`; it is not in the Operation log. It is loaded when the own database is made, and when the file's content is not the one loaded last: then **the own database is made anew first** (emptied, its migrations applied again, so `migrations_applied` names every one of them), since rows cannot be loaded twice into tables that have them.
- **A statement that fails** ends the loading: reason `test-rows-failed`, exit code `1`, with `file`, `statement` (its first line) and `error` (the database's message) beside the message; nothing of that file is loaded (a stand-in is left empty; the own database is left as the migrations make it), and the next run tries again. `vibe dev` answers requests with its page for a project that does not build until the file is corrected.
- The output of `vibe request` gains `"test_rows": [ { "file": "test-rows/devops.sql", "rows": <the rows the file's statements changed> } ]`: the files this run loaded, in name order; empty when it loaded none. In words, one line for each.
- The statements are split by Postgres's own scanner. Test rows are the Builder's: they are never sent anywhere, and a deploy does not read them.

### Starting over

```
vibe local reset [--project <directory>] [--timeout <seconds>] [--json]
```

- It removes everything the local runs made in the project: the local own database, the stand-ins, the local Versions, the local Operation log, Change history and logs. Then it does a run's work again up to the request: a new private Postgres, the stand-ins, the Version (numbered from 1), every migration, the test rows. It prints `{ "reset": true, "application", "version": 1, "migrations_applied": [ ... ], "standins": [ ... ], "test_rows": [ ... ], "warnings", "notes" }`; in words, what it did. Its failures are a run's.
- The programs of Postgres in the user's own folder are not touched, and nothing is downloaded again. `vibe.dev.json`, the test rows and every other file of the project are not touched.
- While `vibe dev` runs, the reset is done by it: requests under way are finished, those that come meanwhile wait, and it goes on serving at the same address afterwards. The chosen test User stays chosen.
- A project that never had a local run: the same, from nothing.

### Without a connection

- With the structure pulled, the packages installed and Postgres in the user's own folder (or `VIBE_LOCAL_POSTGRES_BIN`), `vibe dev`, `vibe request`, `vibe local operation-log`, `vibe local change-history` and `vibe local reset` open no connection but to the private Postgres and their own loopback address: no name is looked up, the installation is not asked, no sign-in is needed, and nothing is downloaded. They work with the installation stopped and with `VIBE_LOCAL_POSTGRES_MIRROR` naming an address that does not answer.

### The instruction file

From this ticket on the template's `AGENTS.md` names `vibe dev` and `vibe local reset` in its table of commands, and says: that `vibe dev` serves the project at `http://<name>.localhost:7480/` for the Builder's browser, where the Builder chooses the test User, and that the agent's own `vibe request` is answered by it while it runs; that test rows are SQL files in `test-rows/` (one a pulled Resource, and `own-database.sql`), loaded when they change; and that `vibe local reset` starts the local databases over.

### What this does not close

- Static files, and the limits of a request in full: their tickets.
- Windows, where the system does not resolve `*.localhost`: the ticket on the three systems.
- The chosen test User is a cookie anyone on the machine can set: the local run holds test rows only, and listens on the loopback address only.
- A change of a file is seen at the next request, not before: nothing is rebuilt while nobody asks.

## Sign-in and sessions (ticket: OIDC sign-in and per-subdomain sessions)

The platform is the OIDC relying party (ADR 0011, the answer of "Sign-in, sessions and Application URLs"). **From the ticket on the tool's sign-in through the browser on**, the issuer, the client id, the client secret, the Groups claim and the scopes are no longer given to `vibe serve`: they are the connection a Platform administrator makes with `vibe admin connect-oidc` ("The tool signs in through the browser, and OIDC is connected from the tool"). What each of them means, and everything else in this section, stands. A session lives on the platform's side; a browser holds opaque cookies, one for the platform's sign-in address and one for each Application's address, none of them valid anywhere else. The stand-in sign-in stays for tests and the local run only.

### Starting with OIDC

```
VIBE_OIDC_CLIENT_SECRET=<secret> \
vibe serve --domain <domain> --listen <ip:port> \
         --oidc-issuer <URL> --oidc-client-id <id> \
         [--oidc-groups-claim <name>] [--oidc-scopes "<scope> <scope> ..."] \
         [--public-scheme <https|http>] [--public-port <n>] \
         [--session-renewal <seconds>] [--session-lifetime <seconds>]
```

- `--oidc-issuer` is the identity provider's issuer, `https://...`; `http://` is taken only for a loopback host (`127.0.0.1`, `[::1]`, `localhost`). `--oidc-client-id` and `VIBE_OIDC_CLIENT_SECRET` (in the environment, never on the command line) are the platform's registration there. The two options need each other and the secret: one of them without the other, or both without the secret, or an issuer that is not such a URL, and the start-up fails as "Serving" says, naming each thing that is missing. The secret in the environment without the options is not looked at. The secret is in no log and no message.
- `--oidc-groups-claim` names the claim of the ID token that holds the person's Groups (`groups` without it): an array of strings, or one string (one Group). Absent, or of another shape: no Groups. Entries are trimmed, empty ones dropped, repeats kept once, the order kept; at most 1,000 Groups of at most 256 bytes each (more, or longer: the sign-in fails, below).
- `--oidc-scopes` are the scopes asked for (`openid profile email offline_access` without it); `openid` is always among them.
- `--public-scheme` and `--public-port` say how a browser reaches the installation, whatever the sign-in, for the addresses the platform writes into redirects and compares an `Origin` with: `https` and `443` without them; with `--public-scheme http`, the port without `--public-port` is the one the program listens on. The platform's sign-in address is `<scheme>://auth.<domain>[:<port>]`, an Application's is `<scheme>://<application>.<domain>[:<port>]` (the port left out when it is the scheme's own). The redirect address registered at the identity provider is `<sign-in address>/callback`.
- `--session-renewal` (1 to 86400 seconds; `600` without it) and `--session-lifetime` (60 to 2592000; `43200` without it): below.
- **The stand-in is for tests and the local run.** `--stand-in-sign-in` together with `--oidc-issuer` is a start-up failure, and so is `--stand-in-sign-in` with a `--listen` address that is not a loopback address. The listening line says `"sign_in": "oidc" | "stand-in" | "none"` beside `stand_in_sign_in`.
- The identity provider is not asked anything at start-up: the program starts with it unreachable. Its configuration (`<issuer>/.well-known/openid-configuration`) and its keys are fetched when first needed and kept for at most an hour (the keys are fetched again, at most once a minute, when a token names a key not among them). Every call to it ends within 10 seconds; its answers are read up to 1 MiB.

### The way through sign-in

1. **A request to an Application without a session there** (no cookie, or one that is not valid): a `GET` or `HEAD` whose `Accept` has `text/html` is answered `302` to `<sign-in address>/sign-in?application=<name>&next=<the path and query, percent-encoded>&state=<a value made for this browser>`, with a short-lived cookie holding that value on the Application's address; any other request is answered `401`, `text/plain`, and the Application's code is not run. This is "Not signed in" of "Responses the program itself gives", so it comes before the Application is looked up: an address that is no Application's is answered the same, and someone who is not signed in cannot learn which Applications exist.
2. **`GET <sign-in address>/sign-in`**. `application` is a name an Application could have (else `400`), `state` is the value of step 1, at most 128 bytes (missing, empty or longer: `400`), `next` begins with one `/` and not with `//` or `/\` (else it is `/`). With a valid platform session in this browser, and `application` not a known Application: `404`, a page of the platform's own (only someone signed in learns it). With a valid platform session and a known Application: `302` to `<the Application's address>/_vm/session?code=<a one-time code>&state=<as given>&next=<next>`. Without: `302` to the identity provider's authorization endpoint, an authorization-code request with PKCE (`S256`), `state` and `nonce`, the scopes above and the redirect address above; on this way a `next` longer than 1,024 bytes is `/`.
3. **`GET <sign-in address>/callback`** takes the provider's answer. The code is exchanged at the token endpoint (the client authenticated with `client_secret_basic`, or `client_secret_post` when the provider's configuration names only that); the ID token must be signed with `RS256` or `ES256` by a key of the provider's, and its `iss`, `aud`, `exp` and `nonce` must hold (30 seconds of clock difference are allowed; a time given with a fraction of a second is read as its whole seconds). Then the User's record is made if this is the first time, a platform session begins, its cookie is set on the sign-in address, and the browser goes on as in 2. Anything else: a page of the platform's own, saying what failed and giving a link to try again at the Application's address when it is known which Application the sign-in was for (else the page says to open the Application again, and links nowhere); no session begins, and nothing of a token is shown. It is `502` when the provider could not be reached, did not answer in time, answered `408`, `429` or `5xx`, or answered what cannot be read (larger than the bound, not JSON, without an ID token; a configuration that names another issuer or lacks an endpoint; keys that cannot be read), in step 2 as well, and the page says which of these it was; it is `400` for everything else (a `state` this browser did not start, an `error` from the provider, a code the provider refuses, an ID token that does not hold, too many Groups or one too long, counted once repeats are dropped).
4. **`GET <application address>/_vm/session`** takes the one-time code: it was made for this Application, is at most 60 seconds old, was not used before, and `state` is the value of this browser's cookie from 1 (else `400`, a page of the platform's own; nothing is set). A code is used up by the first request that shows it, whatever comes of that request. Then the Application's own session cookie is set and the browser is sent (`302`) to `next`. The cookie of step 1 is left to run out, so that two pages of one Application that sign in at once in one browser both land. This path is the platform's on every Application's address and never reaches Application code; every other path under `/_vm/` is the Application's, as before.

- **One sign-in for all**: a browser that has a platform session goes through 1, 2 and 4 for a second Application without seeing the identity provider.
- A redirect of the platform's never leaves the installation but in step 2's second case: `next` is only ever a path, `application` only ever a name.
- At most 10 minutes may pass between 2 and 3. **The platform keeps nothing of a sign-in under way.** The `state` it sends to the identity provider holds what the callback needs (where the browser goes afterwards, and what the token is checked against), sealed with a key of the platform's own, so that nobody else can read one or make one, and good only in the browser that began it (a cookie on the sign-in address says which). So sign-ins begun and never finished, however many, stop no other; and one under way goes on when the program is started again meanwhile with the same key (the client secret's until the ticket on the tool's sign-in through the browser, the installation's from then on).

### The cookies

- The platform session's cookie is set on the sign-in address only; an Application's on that Application's address only. Neither has a `Domain` attribute: **no cookie spans the wildcard domain.** Each is `HttpOnly; SameSite=Lax; Path=/`, and with `--public-scheme https` also `Secure` and named with the `__Host-` prefix (`__Host-vm-session`; `vm-session` with `http`). The cookie of step 1 is the same but for its name (`__Host-vm-sign-in` or `vm-sign-in`) and its age (10 minutes).
- Their values are random (256 bits; step 1's too) and mean nothing: the platform database holds only their SHA-256. Their names are matched exactly, letter case included.
- **An Application's cookie is valid on that Application only.** Sent to another Application's address, or to the sign-in address, it is no session.
- **Application code never sees them**: the platform's cookies (`vm-session`, `vm-sign-in`, with or without `__Host-`) are taken out of the `Cookie` header before the request reaches the code (other cookies stay, byte for byte; a header left empty is left out). Nor can it set them: a `Set-Cookie` of an Application's response that names one of them, or that has a `Domain` attribute (it would be sent to the other Applications), is dropped, and the program's log gets `{"event":"set-cookie-dropped","application","reason":"platform-name"|"domain"}`.

### Whose request it is

- A request with a valid Application session is given to the code as the session's User: `id` is the `sub` of the ID token, `groups` are the session's Groups. `x-vm-stand-in-*` headers mean nothing.
- **A request from another origin is refused**, `403`, `text/plain`, the code not run, the Operation log untouched:
  - a request whose method is not `GET`, `HEAD` or `OPTIONS`, when it has an `Origin` that is not the Application's address, or has no `Origin` and a `Sec-Fetch-Site` other than `same-origin` or `none` (with both headers, `Origin` decides);
  - a `GET` or `HEAD` with an `Origin` that is not the Application's address, or whose `Sec-Fetch-Site` is `cross-site` or `same-site`, unless it is a navigation of the whole page (`Sec-Fetch-Mode: navigate` and `Sec-Fetch-Dest: document`, and no `Origin`);
  - an `OPTIONS` with an `Origin` that is not the Application's address.
  **An `Origin` of `null` together with `Sec-Fetch-Site: same-origin` is the Application's own origin** in all three (from the ticket "The Dashboard" on): it is what a browser sends with a form posted from a page of that same origin whose referrer policy is `no-referrer`, which an Application's own pages may well have, and their forms must work. `Origin: null` with any other `Sec-Fetch-Site`, or with none, is another origin as before. (A browser sets `Sec-Fetch-Site` itself and no page can; it says `same-origin` only when the page that made the request, and every redirect on the way, is of this very origin. A page in a sandboxed frame or at a `data:` address has no origin, and its requests are `cross-site`.)
  A request with neither header is no browser's and is taken as before. This holds with the stand-in sign-in too. So one Application's page cannot call another as the same User, cannot post a form to it, and cannot show it in a frame (the last in a browser that sends `Sec-Fetch-Site`, as every current one does).
- The check comes after the sign-in in "Responses the program itself gives": a request with no session is answered as in 1 whatever its origin.
- The same rule, with the same exception, guards the sign-in address (its forms: sign-out, the tool's confirmation) and the Dashboard's, each with its own address where the Application's is.
- **A form of the platform's own pages, submitted as a browser submits it, is taken.** A browser sends with it `Origin` (the page's origin, or `null` when the page's referrer policy is `no-referrer`), `Sec-Fetch-Site: same-origin`, `Sec-Fetch-Mode: navigate` and `Sec-Fetch-Dest: document`; the tests submit every form of the platform's so, reading the policy from the page that holds the form. Until the ticket "The Dashboard" the platform's pages had `referrer-policy: no-referrer` and the rule refused `null`: sign-out and the tool's confirmation were refused in a real browser (seen in Firefox 159), though every test passed, each sending the `Origin` a browser would not.

### Renewal and the end of a session

- **Groups are read again at each renewal.** When a request finds its session last renewed more than `--session-renewal` ago, the platform first asks the identity provider with the session's refresh token. A new ID token that holds (its signature, `iss`, `aud` and `exp`; a `nonce` is not asked of it): the session's Groups are those of the new token from that request on, in every Application. A refusal (the token endpoint answers `4xx` other than `408` and `429`: `invalid_grant` when the person was disabled or the token revoked), an answer without an ID token, an ID token that does not hold, or one of another `sub`: the session ends. A provider that does not answer (not reached, late, `408`, `429`, `5xx`, an answer that cannot be read): the request is answered `503` with `Retry-After`, the code not run, and the session stays; the Groups read before are never used past their time.
- **One renewal serves every request of its session that comes meanwhile**: they wait for it and take its outcome, whatever it is, and the provider is asked once. A renewal ends within 10 seconds, so each of them is answered within about that. It is the platform's work and not the request's: it goes on to its end when the request that began it goes away, so a refresh token the provider replaced meanwhile is not lost. At most 256 sessions are being renewed at once, a quarter of the 1,024 connections the program serves at once, so that requests waiting for a provider that does not answer never take them all: a request whose session's renewal cannot begin is answered `503` at once, and the requests of other sessions, of nobody signed in and to the API go on being answered meanwhile.
- A session's refresh token is kept sealed, with a key made from the installation's key (`VIBE_SECRET_KEY`, which is not in the platform database; until the ticket on the tool's sign-in through the browser it was made from the client secret). A session whose refresh token no longer opens (the installation's key was changed and OIDC connected again) ends at its next renewal.
- A session whose sign-in brought no refresh token ends at its first renewal.
- A session ends, for every Application at once: at `--session-lifetime` after its sign-in; at a renewal as above; at sign-out. A request with an ended session is one without a session (1 above): a page goes through sign-in again, which the identity provider may answer without asking.
- **Sign-out.** `GET <sign-in address>/sign-out` is a page with one button; `POST <sign-in address>/sign-out` (same origin, by the rule above) ends the platform session and every Application session made from it, clears the cookie, and answers a page saying so, and that the company's own sign-in may still be open. The identity provider is not told.
- A User holds at most 20 sessions: the oldest ends when a twenty-first begins. A session that has ended is deleted from the platform database, its refresh token with it, at the latest when anyone next signs in.
- **What the program's log says.** Why a session ended at a renewal: `{"event":"session-ended","session","reason":"refused"|"no-refresh-token"|"unreadable-refresh-token"|"no-id-token"|"invalid-id-token"|"another-sub"}`. Each call to the identity provider that brought no answer it could use: `{"event":"identity-provider-unanswered","call":"configuration"|"keys"|"token","problem":"unreachable"|"timed-out"|"too-large"|"status"|"not-json"|"no-id-token"|"another-issuer"|"lacks-endpoint"|"unreadable-keys","status"}` (`status` the answer's, when it had one). A refusal of the token endpoint: `{"event":"identity-provider-refused","status","error"}` (`error` its code when that is a short token of letters, digits and `_.-`; never its description). Nothing of a token, a code, a cookie or a secret is in any of them.

### The User's record

- Made at the first sign-in: the `sub` as `id`, with `name` and `email` from the ID token when it has them (each at most 256 bytes, else left out), and the time. `name` and `email` are brought up to date at each sign-in and renewal.
- `GET <sign-in address>/me`: `200`, `application/json`, `{ "id", "name", "email", "groups", "first_sign_in", "session_started", "session_renewed" }` for this browser's platform session (the three times as everywhere in the API: RFC 3339, UTC; `name` and `email` null when there is none); `401` without one. Every other path of the sign-in address is `404`.

### The harness

The black-box harness has an identity provider of its own (`blackbox`, no product crate): an issuer on a loopback address with test people, their Groups, a signing key, and switches a test turns (a person's Groups changed, a person disabled, the provider not answering, a token signed with another key, no refresh token).

### What this does not close

- The tool's own sign-in (`vibe sign-in`) still uses the stand-in: the ticket "The tool signs in through the browser" (closed there).
- Whom an Application is shared with: the ticket on Contributors and sharing ("Contributors and sharing: Opening an Application"). Until then every signed-in User may open every Application.
- The Dashboard at `dashboard.<domain>`: its ticket; it signs in by the same way through ("The Dashboard: Signing in to it").
- Signing out at the identity provider, and being signed out by it (back-channel logout).
- TLS: the program listens in plain HTTP behind what ends TLS: the Compose file's proxy or the administrator's own ("Installing: one image and a Compose file"). The program serving TLS itself is the ticket "The program serves TLS itself with a supplied wildcard certificate".
- An identity provider whose certificate is signed by a company's own certificate authority: closed since, by `VIBE_CA_FILE` ("A company's own certificate authority").

## The tool signs in through the browser, and OIDC is connected from the tool (ticket: The tool signs in through the browser; Platform administrators come from the directory)

From this ticket on OIDC is not a way of starting the program: it is a **connection** a Platform administrator makes from the tool, kept in the platform database (ADR 0018). Nobody can sign in before it is made, so the first one is made with a one-time **setup credential** that whoever installs the platform prints. The tool then signs in through the Builder's browser, and Platform administrators are the members of the Group the connection names.

### The installation's key

- `VIBE_SECRET_KEY`, in the environment of `vibe serve` and never on the command line: 32 random bytes in base64 (`openssl rand -base64 32`). Set to anything else (empty included): a start-up failure as "Serving" says, whose message names `VIBE_SECRET_KEY` and never shows the value.
- With it the platform seals what it keeps secret in the platform database (AES-256-GCM, a key of its own for each use, made from this one): the client secret of the OIDC connection, each session's and each tool credential's refresh token, and the `state` of a sign-in under way. This replaces the key made from the client secret in "Sign-in and sessions": a changed client secret ends no session. A dump of the platform database holds none of them in a form that can be used.
- Without it the program serves all the same, but OIDC cannot be connected (`409` `no-secret-key`), and a connection that is in the platform database is not used: the listening line says `"sign_in":"none"` and the log gets `{"event":"oidc-connection-unusable","reason":"no-secret-key"}`. With another key than the one the connection was made under: the same, with `"reason":"another-secret-key"`. `GET /v1/installation` says `none` in both. Then OIDC is connected again (below).

### The setup credential

```
VIBE_DATABASE_URL=postgres://... vibe setup [--json]
```

- Run by whoever installs the platform, beside the platform database, whether the program is running or not (it brings the platform database's structure up to date first, as `vibe serve` does). It makes **the setup credential** and prints it, once: `{ "setup_credential": "vmsc_<...>", "expires_at": "<RFC 3339 time>" }` with `--json`; without, the credential alone on standard output and what to do with it on standard error (naming `vibe admin connect-oidc`). The platform database keeps only its SHA-256. It is 32 random bytes or more.
- It is good for 24 hours and for one thing: connecting OIDC (below), once. Any other request made with it is `401`. A second `vibe setup` ends the first one's credential.
- It is also the way back in when nobody can sign in (a connection that turned out wrong, a lost key): `vibe setup` again, by someone who has the platform database's address. Its failures are those of a start-up: exit code `1` and a message that never shows a password; without `VIBE_DATABASE_URL`, or with one that cannot be read, the message names `VIBE_DATABASE_URL`.

### Connecting OIDC

```
VIBE_SETUP_CREDENTIAL=vmsc_... VIBE_OIDC_CLIENT_SECRET=<secret> \
vibe admin connect-oidc --domain <domain> --connect <ip:port> \
    --issuer <URL> --client-id <id> --administrators-group <Group> \
    [--groups-claim <name>] [--scopes "<scope> <scope> ..."] [--json]
```

- `VIBE_OIDC_CLIENT_SECRET`, in the tool's environment and never on the command line, is required (without it: exit code `2`). With `VIBE_SETUP_CREDENTIAL` in the environment the command is made with the setup credential, and `--domain` says where (required then: without it, exit code `2`); the installation is reached as `vibe sign-in` reaches it ("Where the tool connects"): over HTTPS, with `--port` and `--resolve` when given, or over plain HTTP at a loopback `--connect` (until the ticket of that section `--connect` was required here too). Without it the command is made with the tool's own credential, at the installation it is signed in to and the way the credential file keeps (`--domain`, `--port`, `--resolve` and `--connect` are each exit code `2` then), and its User must be a Platform administrator.
- `--issuer`, `--client-id`, `--groups-claim` (`groups` without it) and `--scopes` (`openid profile email offline_access` without it; `openid` always among them) mean what "Sign-in and sessions: Starting with OIDC" said of the options of `vibe serve` that are gone now. `--administrators-group` names the Group whose members are Platform administrators (1 to 256 bytes, no control characters).
- It prints the connection: `{ "issuer", "client_id", "groups_claim", "scopes": [...], "administrators_group", "connected_at", "connected_by" }` (`connected_by` is the User's id, or `setup-credential`). Neither secret is ever printed, and neither is kept on the Builder's machine.
- Reasons of its own: `setup-credential-refused` (exit code `3`: the installation answered `401` to the setup credential; the message says it is used up, replaced or expired and to run `vibe setup` again). The API's refusals keep their reasons, as for every command.

On the platform's API:

- `PUT /v1/oidc`, by a Platform administrator, or with `Authorization: Bearer <the setup credential>`: `{ "issuer", "client_id", "client_secret", "administrators_group", "groups_claim"?, "scopes"? }` (`scopes` one string, separated by spaces). Answers `200` with the connection as above.
  - `400` `invalid-issuer` for an issuer that is not `https://...` or `http://` on a loopback host (no user, query or fragment); `400` `malformed` for the rest (a `client_id` or `client_secret` that is empty, longer than 256 bytes (the secret: 1,024) or has control characters, a Group that is no Group, an empty `groups_claim`).
  - `409` `stand-in-sign-in` when the program was started with `--stand-in-sign-in`; `409` `no-secret-key` when it has no `VIBE_SECRET_KEY`.
  - In this order: who asks (`401`, `403`: a setup credential that is not the live one is `401` whatever else is wrong), then the two `409` (the stand-in first), then the body (`400`; a member that is missing or of another type, and a body that is not JSON, are `malformed`, `invalid-issuer` is for an issuer that is a string), then the provider (`422`). A refusal changes nothing and leaves the setup credential good.
  - **The provider is asked before the connection is taken**: the platform fetches the issuer's configuration (within 10 seconds, read up to 1 MiB). When it cannot be used (not reached, late, not JSON, another issuer, an endpoint missing or one that cannot be used): `422` `identity-provider-unusable`, the message says which of these it was, and nothing is changed. **An endpoint's scheme follows the issuer's**: with an `https` issuer each of the three endpoints the platform needs (authorization, token, keys) must be `https`, on whatever host, a loopback one included; an `http` endpoint is taken only with an `http` issuer, which is a loopback one. A configuration that names an `http` endpoint under an `https` issuer cannot be used, at `PUT /v1/oidc` and at a sign-in alike (there it is the `502` of step 3 of "The way through sign-in", logged as `lacks-endpoint`). The client secret cannot be checked this way: a wrong one shows at the first sign-in.
  - It takes effect at once, in the running program: the sign-in address serves, and `GET /v1/installation` says `oidc`. The log gets `{"event":"oidc-connected","issuer","client_id","administrators_group","by"}` and never a secret.
  - Made with the setup credential, it ends that credential in the same act: used again, it is `401`.
  - A connection made over an earlier one: when the issuer or the client id differ, every session and every tool credential that came from a sign-in ends (they were another provider's people). Otherwise they stay, and their next renewal is made with the new settings; who is a Platform administrator follows the new Group at once.
- `GET /v1/oidc`, by a Platform administrator: the connection; `404` `not-found` when there is none.
- `GET /v1/installation`, by anyone, signed in or not: `{ "domain", "sign_in": "oidc" | "stand-in" | "none", "sign_in_address": "<scheme>://auth.<domain>[:<port>]" | null }` (the address only with `oidc`).

### Starting, from this ticket on

- `vibe serve` no longer has `--oidc-issuer`, `--oidc-client-id`, `--oidc-groups-claim` and `--oidc-scopes` (each is exit code `2`, like any option it does not have), and does not look at `VIBE_OIDC_CLIENT_SECRET`. `--public-scheme`, `--public-port`, `--session-renewal` and `--session-lifetime` stay as they are. At its start the program uses the connection the platform database holds: the listening line says `"sign_in":"oidc"` then, and the provider is still asked nothing before someone signs in.
- `--stand-in-sign-in` on a platform database that holds a connection is a start-up failure whose message names `--stand-in-sign-in` (the stand-in cannot be on together with OIDC), whether or not the connection can be used with the key given.
- `--administrators-group` belongs to the stand-in: given without `--stand-in-sign-in` it is a start-up failure whose message names `vibe admin connect-oidc`.
- **A tool credential made with the stand-in signs nobody in at a program started without `--stand-in-sign-in`**: `401`, as one that is not known (it states its own User and Groups, which nobody checked, so it must not outlive the stand-in and become a Platform administrator's once OIDC is connected). It is good again at a start with the stand-in, until its lifetime.
- The bounds of `--session-renewal` and `--session-lifetime` are checked at every start, whatever the sign-in.

### Who is a Platform administrator

- With OIDC: a User whose Groups, as last read from the provider, hold the connection's administrators' Group. A session or a tool credential of someone taken out of that Group at the provider stops being an administrator's at its next renewal, and of someone put into it becomes one then. Everyone else is refused (`403` `not-an-administrator`) as before.

### Signing the tool in

```
vibe sign-in --domain <domain> --connect <ip:port> [--no-browser] [--timeout <seconds>] [--json]
```

- The tool asks `GET /v1/installation`. `none`: `sign-in-refused` (exit code `3`), the message says that OIDC is not connected yet and that a Platform administrator connects it with `vibe admin connect-oidc`. `stand-in`: as before, with `--stand-in-user` (without it: `sign-in-refused`, saying so). `oidc`: through the browser, as follows; `--stand-in-user` is `sign-in-refused` there.
- The tool makes a **verifier** (256 random bits, kept in its memory only) and sends its **challenge** (`BASE64URL(SHA-256(verifier))`) through the browser. Nothing is copied by hand, and the Builder never sees a credential.
- **With a browser on the machine**: the tool listens on `127.0.0.1:<a free port>` and opens `<sign-in address>/tool?challenge=<challenge>&port=<port>&machine=<the machine's host name>`: with the command in `BROWSER` when that is set (the address is its last argument), else with the system's own (`xdg-open`, `open`). It also writes the address to standard error, for a browser that did not open. **The tool does not wait for that command to end** (it may be the browser itself, as with `BROWSER=firefox`): it listens and goes on at once, the browser is answered at its port as fast as with a command that ends, and a command still running when the sign-in ends is left running.
- **Without one** (`--no-browser`; or no `BROWSER`, and on Linux neither `DISPLAY` nor `WAYLAND_DISPLAY`): it writes to standard error the address `<sign-in address>/tool?challenge=<challenge>&machine=<name>`, to open on any device, and a **check code** of eight characters (`XXXX-XXXX`: digits and capital letters without `I`, `L`, `O` and `U`) that the page will ask for. It is made from the challenge: the first 40 bits of SHA-256 of the 43 bytes `vm: the check code of a sign-in of the tool`, one zero byte and the challenge's 32 bytes, five bits a character, the first bits first, in the alphabet `0123456789ABCDEFGHJKMNPQRSTVWXYZ`. The page takes it in either case, with or without the hyphen and spaces, and never shows it.
- At the sign-in address:
  1. `GET /tool` with a `challenge` of 43 base64url characters, a `port` of 1024 to 65535 when there is one, and a `machine` of at most 64 bytes (control characters dropped): `302` to the identity provider, an authorization of its own (so the tool's credential has its own refresh token), sealed into `state` as every sign-in under way. Anything else, one of the three given twice included: `400`, a page of the platform's own. Other parameters are not looked at.
  2. Back at `/callback`, signed in at the provider: a page that says who is being signed in (name, email, id), on which machine, and asks. With a `port` it says the tool is on this computer; without, it asks for the check code the tool shows. One form, `POST /tool` (`application/x-www-form-urlencoded`), same origin by the rule of "Whose request it is": a hidden `sign_in` (which sign-in awaiting an answer; of no use without the browser's cookie of the sign-in address it was made for), a text input `code` when there is no `port`, and two submit buttons named `answer`, with the values `confirm` and `decline`. The page says to go on only if the person ran `vibe sign-in` themselves.
  3. `POST /tool`, confirmed: with a `port`, the browser is sent to `http://127.0.0.1:<port>/?grant=<a one-time value>` (never to another host), where the tool answers a page saying it is signed in (it needs only the request's first line, and reads a head of up to 64 KiB within 5 seconds: a browser sends there the cookies other programs on `127.0.0.1` set; to anything but `GET /?grant=<...>` it answers `404`); without, a page (`200`) says so and to go back to the tool. A wrong check code shows the page again (`200`, with its form); the fifth ends the sign-in. Declined: a page (`200`) says nothing was signed in, and the sign-in is over. A `POST /tool` for a sign-in that is over or not known (answered already, five wrong codes, pushed out by a sixth, 10 minutes gone, another browser): `400`, a page with no form that says to run `vibe sign-in` again. Anything else that is not such a form: `400`.
  - The platform keeps something only from step 2 on, when someone has signed in at the provider: at most 5 sign-ins of the tool awaiting an answer for each User (the oldest goes), each for 10 minutes. Before that there is nothing of the platform's to use up.
- `POST /v1/tool-sign-ins`, by anyone, with `{ "verifier": "<...>", "grant": "<...>"? }`: `201` with the answer of `POST /v1/tool-credentials` (the credential, once) when the sign-in whose challenge this verifier makes has been confirmed, and, if it was begun with a `port`, the `grant` is the one the browser was sent to the tool with. **The two ways are kept apart**: a request that carries a `grant`, whatever it is, takes only a sign-in begun with a `port`, and one without takes only a sign-in begun without (so nothing sent to the port of a tool that waits there, by a page in the browser or a program on the machine, makes it take a sign-in confirmed elsewhere with a check code). Until then, for a verifier nothing is known of, and once the credential has been given (the same verifier asked again): `409` `sign-in-not-confirmed`. A body that is not such JSON: `400` `malformed`. So a link sent to someone else gains nothing: without a `port` they must type a code only the sender's tool shows, with one the grant goes to their own machine. The other way round, someone who has seen the address of a sign-in begun without a port can make its check code and sign that tool in as themselves, never as anyone else: the tool says whom it was signed in as, and the address is the Builder's to keep.
- Without a port the tool asks every 2 seconds. **With a port it asks only with a grant its port was given**: at once when the browser reaches the port, then every 2 seconds with that grant while the answer is `409`; it never asks without one, so a sign-in confirmed elsewhere under the same challenge and no port does not sign it in. Either way for at most `--timeout` seconds (10 to 600; `300` without it). Then: `sign-in-not-finished`, exit code `3`, the message gives the address again and says to run the command again when the Builder is ready. Nothing is kept.
- What it prints and keeps is as in "The tool: Signing in" and "The credential". The credential's User is the `sub`; **its Groups are read again at each renewal, like a session's** (`--session-renewal`), with its own refresh token: a refusal of the provider ends it (the usual `401`; the tool says `sign-in-expired`), a provider that does not answer is `503` `unavailable` and the credential stays, and one whose sign-in brought no refresh token ends at its first renewal. `GET /v1/me` and `vibe whoami` give the Groups as last read. It still ends at its lifetime, at `vibe sign-out`, and when its User makes a seventeenth.

### The instruction file

- `AGENTS.md` of a new project tells the Coding agent that when a command says it is not signed in, it runs the `vibe sign-in` command the message gives, which opens the Builder's browser (or shows an address for the Builder to open), tells the Builder so, and runs its own command again after; and that `sign-in-not-finished` means the Builder has not answered yet: ask them, then run `vibe sign-in` again. It never asks the Builder for a password or a token.

### The harness

The harness connects OIDC as an administrator would (`vibe setup`, then the connection), walks a browser through `/tool`, and plays the opened browser with a `BROWSER` command of its own.

### What this does not close

- Other commands for Platform administrators: the tickets on Resources and Grants in full and on what Platform administrators see.
- A Platform administrator ending someone else's tool credential or session.
- The tool spoke plain HTTP to a loopback address only; closed since ("The tool reaches an installation over HTTPS"). The Dashboard: its ticket.

## Contributors and sharing (ticket: Contributors and sharing: who may change and who may open an Application)

An Application belongs to the company (ADR 0007). Two lists are kept for each: its **Contributors**, who may change and deploy it, and whom it is **shared with**, who may open it. Until it is shared it is visible to its Contributors alone. Sharing never widens anyone's access to a shared database: that stays with their Grants.

### The two lists

- An entry of the Contributors names a person or a Group: `{ "user": "<user id>" }` or `{ "group": "<Group>" }`. An entry of whom it is shared with is one of those, or `{ "everyone": true }`: every User who can sign in to the installation.
- A user id and a Group are each 1 to 256 bytes with no control characters, compared exactly (as a Grant's Group). Nothing checks that such a person or Group exists: an Application can be shared with someone before their first sign-in.
- **A User is a Contributor** of an Application when their id is among its Contributors or one of their Groups is. **A User can open** an Application when they are a Contributor of it, or it is shared with everyone, with them, or with one of their Groups. Their Groups are the ones their sign-in holds, as last read ("Sign-in and sessions: Renewal"): taken out of a Group at the identity provider, a person stops being a Contributor or stops being able to open at the next renewal.
- The creator is the first Contributor (`{ "user": ... }`). The Contributors are never empty.
- At most 100 Contributors and 1,000 entries of whom it is shared with.
- Wherever this contract says "by a Contributor" or `not-a-contributor`, this is who is meant from this ticket on: sending a Version, going back, reading Versions and logs, deploying with the tool.
- **An Application that was there before this ticket** keeps its Contributors, each as added by its creator when it was created, and is shared with nobody: from the first start of the program of this ticket on it is opened by its Contributors alone, until one of them shares it.

### Opening an Application

In "Responses the program itself gives", after the `404` of an address that is no known Application's and before the `404` of an Application with no serving Version:

| Condition | Status |
|---|---|
| The Application is not shared with the User, and the User is not a Contributor of it | 403 |

- The Application is not asked anything: none of its code runs, none of its files is given, whatever the path and the method. The `request` record has the Application's name, `version` null and `status` 403.
- A `GET` or `HEAD` whose `Accept` has `text/html` is answered with a page of the platform's own (`text/html; charset=utf-8`, with `cache-control: no-store`, `x-content-type-options: nosniff`, `referrer-policy: same-origin` (`no-referrer` until the ticket "The Dashboard"; see below) and a `content-security-policy` that has `default-src 'none'` and `frame-ancestors 'none'`, as every page of the platform's own; a `HEAD` gets them without a body): it says that the Application of that name is not shared with the person, names whom they are signed in as (their id, and their name when the platform knows it), and says to ask one of the Application's Contributors to share it. It names no Contributor and nothing else of the Application, and everything it shows of the person is escaped as HTML. With OIDC it has one link, to `<sign-in address>/sign-out` (the whole address, scheme and port included: the Application's own `/sign-out` is not the platform's), for someone signed in as the wrong person; with the stand-in, which has no sign-out, it has no link at all. Any other request: `text/plain`, 1 to 200 bytes, one sentence that names the Application and says it is not shared with the User.
- So someone signed in can learn that an Application of a name exists (`403` and not `404`), as they can from `name-taken`; nothing more.
- It is decided at each request: sharing and unsharing take effect at the next one, with nothing to wait for and no sign-in again. Once the `200` of a change has been received, every new request is decided by the lists as that change left them; changes made at once leave the program deciding by the lists as the API answers them afterwards.
- It is decided from the two lists as the program holds them in memory, in step with the platform database like Grants ("The program decides from what it holds in memory"): it adds no round trip to the platform database to a request, and while the platform database cannot be reached whoever could open still opens and anyone else still gets the `403`. (With OIDC a session is still read from the platform database, as before.) A change that is answered `503` `unavailable` may or may not have been made, and may still be made after that answer (its commit was on its way when the program stopped waiting for it). The program never goes on deciding by the lists as they were while it cannot tell: from that answer on, a request to that Application is `503` (with `retry-after`, soon, and without waiting for the platform database) until the program has read from the platform database what became of the change, which it does at a request once the change is settled; from then on requests are decided by the lists as the database has them. The same at a start: the program does not serve an Application by lists read while a change of them was still on its way. Nobody is let in on a guess.
- With the stand-in every path of the Application's address is under this rule (`/_vm/session` is the Application's own path there).
- With OIDC the steps of a sign-in come first as they do now (a person without a session is sent to sign in before anything is said of the Application), and `/_vm/session` is answered whoever the code is for.
- **The local run does not share**: `vibe request` and `vibe dev` serve every test User of the project, as before.
- A User who opens an Application and holds no Grant on a Resource it uses is refused by that Resource exactly as before (`no-grant`, with its Operation log entry): being able to open gives nothing there.

### On the platform's API

The Application, wherever the API answers it (`POST /v1/applications`, `GET /v1/applications/<name>`, and the two requests below):

```json
{ "name": "incident-board", "description": "...", "address": "incident-board.<domain>",
  "contributors": [ { "user": "<user id>", "name": "<name>" | null, "email": "<address>" | null,
                      "added_by": "<user id>", "added_at": "<RFC 3339 time>" },
                    { "group": "<Group>", "added_by": "...", "added_at": "..." } ],
  "shared_with": [ { "user": ... }, { "group": ... }, { "everyone": true, "added_by": "...", "added_at": "..." } ],
  "contributor": true, "serving_version": 3,
  "created_by": "<user id>", "created_at": "<RFC 3339 time>" }
```

- `contributors` and `shared_with` are in the order they were added; an entry added again keeps its place, its `added_by` and its `added_at`. Every person's entry, in either list, has `name` and `email`; every entry has `added_by` and `added_at` (the creator's entry: the creator and the Application's `created_at`). Wherever an entry is answered (here, in the tool's `added`, `removed` and `visible_to`) it has these members; a removed one is shown as it was kept. `name` and `email` of a person are what the platform last read of them at a sign-in, null for someone who never signed in here (and always with the stand-in, which keeps no record of people). `contributor` says whether the asker is one.
- `GET /v1/applications/<name>`: by whoever can open it, and by a Platform administrator. Anyone else: `403` `not-shared` (after `not-found`), the message saying to ask one of its Contributors. To someone who is neither a Contributor nor a Platform administrator, `shared_with` is null: they see who maintains it, not who else uses it.
- `GET /v1/applications/<name>/versions` and `GET /v1/applications/<name>/versions/<number>`: by a Contributor, and by a Platform administrator, as the logs are; someone it is only shared with uses the Application and does not read how it is made. Anyone else: `403` `not-a-contributor`, after the `not-found` of the Application and `method-not-allowed`, and before anything is said of a Version or of `?limit` (so that a Version's `not-found` tells nobody else how many there are): a number the Application has, one it has not, and a `<number>` that is no number are all that same `403` to them. `vibe versions`: exit code `3`.
- `GET /v1/applications`: the Applications the User is a Contributor of (`contributor` true) and those they can open besides (`contributor` false), by name, each once; exactly those, for a Platform administrator as for anyone. An Application with no serving Version is listed like any other.
- `POST /v1/applications/<name>/contributors` and `POST /v1/applications/<name>/sharing`, by a Contributor, with `{ "add": [ <entry>, ... ], "remove": [ <entry>, ... ] }` (each optional, together 1 to 100 entries): the change is made whole or not at all, and the answer is `200` with the Application as it is after and two more members, `added` and `removed`: the entries this request added and the entries it removed, as kept (a removed one as it was kept), in the order of the list (those added as the request gives them, those removed as they stood in it); both empty when everything was so already. Someone else's change at the same moment is never in them.
  - An entry is `{ "user": "<id>" }`, `{ "group": "<Group>" }`, for `sharing` also `{ "everyone": true }`, or **`{ "email": "<address>" }`**: the one person the platform knows by that email address (as last read at their sign-in; letter case ignored). The entry kept is that person's `{ "user": ... }`.
  - Adding an entry that is there, and removing one that is not, change nothing and are not refused. `400` `malformed`: the same entry in `add` and in `remove` (also when one names a person by `user` and the other the same person by `email`: that one is judged once the emails are resolved, so after `unknown-email` and `ambiguous-email`, and changes nothing); `{ "everyone": true }` for the Contributors (or `"everyone"` with another value than `true`); an entry that names two things or none; a body with neither `add` nor `remove`, with no entry in all, or with more than 100.
  - Refusals, after the ones every request of the API has (`not-found`, `method-not-allowed`, `not-a-contributor`, `malformed`, in that order): `422` `unknown-email` (nobody who signed in here has that address: the message says to use their user id, which `vibe whoami` shows them, or to ask them to sign in once), `409` `ambiguous-email` (more than one person has it: use the user id), `409` `last-contributor` (the change would leave no Contributor), `409` `too-many` (the list as it would be after the whole change has more than 100 Contributors or 1,000 entries of whom it is shared with: one removed and one added at the bound is taken). Each names what it is about and changes nothing: `unknown-email` and `ambiguous-email` have `error.entry`, the `{ "email": ... }` as given; `last-contributor` has `error.entries`, the entries the request would have removed, and `too-many` the entries it would have added, each as the API answers an entry anywhere (a person's with `name` and `email`). Only what the change would really have done is in them: an entry asked to be removed that is not there, or asked to be added that is there, is left out. They are in the order of the list: those removed as they stand in it, those added as the request gives them.
  - A Contributor may remove themselves when a Contributor is left. A Platform administrator who is not a Contributor is `not-a-contributor` here, as anyone.
  - The log gets `{"event":"contributors-changed","application","by","added":[...],"removed":[...]}` or `{"event":"sharing-changed", ...}` with the entries as kept, when something changed.
- Stopping the program and starting it again loses neither list.

### The tool

```
vibe show <application> [--json]
vibe share <application> (--user <id> | --email <address> | --group <Group> | --everyone)... [--json]
vibe unshare <application> (--user <id> | --email <address> | --group <Group> | --everyone)... [--json]
vibe contributors add <application> (--user <id> | --email <address> | --group <Group>)... [--json]
vibe contributors remove <application> (--user <id> | --email <address> | --group <Group>)... [--json]
```

- All need a sign-in (`4`) and name the Application on the command line. Each of `--user`, `--email` and `--group` may be given several times; none given (for the four that change something) is exit code `2`. The API's refusals keep their reasons: exit code `3` for `not-a-contributor`, `not-shared`, `not-found`, `unknown-email`, `ambiguous-email` and `last-contributor`, `1` for the rest.
- **`vibe show`** prints the Application as the API answers it. In words: its name, description, address, the serving Version, its Contributors and whom it is shared with (or, to someone who is not a Contributor, that only its Contributors see that list).
- **The four that change something** print:

  ```json
  { "application": "incident-board", "address": "incident-board.<domain>",
    "added": [ <entry>, ... ], "removed": [ <entry>, ... ],
    "visible_to": { "contributors": [ <entry>, ... ], "shared_with": [ <entry>, ... ] } }
  ```

  `added` and `removed` are what this command changed, as the API answered them (entries as kept: an `--email` shows as the person's `user`, with `name` and `email`), empty when everything was so already. The command is one request: it does not read the Application first. `visible_to.shared_with` is `null` when the API answers null (a Contributor who has just taken themselves off the Contributors), and the words then say that only its Contributors see whom it is shared with. In words: what changed, then **who can see the Application now, each by name**: every Contributor and everyone it is shared with, a person by their name and email when known and by their id, a Group as a Group, and `everyone` as everyone who can sign in to the installation; or that it is visible to its Contributors only. A person the platform has never seen sign in is said so beside their id (a mistyped id shows there).
- **`vibe deploy`**: `visible_to` is that same object from this ticket on (`contributors` were user ids before), and the words name its Contributors and whom it is shared with (people, Groups, everyone), or say that it is visible to its Contributors only and that `vibe share` shares it; this replaces the sentence of "Deploying: What it prints" that the words name no Group and nobody else. Step 4 of "Deploying: The command" still ends a deploy to a name whose Application the User is not a Contributor of with `not-a-contributor`, exit code `3`, before anything is built or sent: when the User can open that Application the message names its Contributors as before; when they cannot (the API answers `not-shared`), it says that an Application of that name exists and is not shared with them, names no Contributor, and says to choose another `name` in `manifest.ts` or to ask whoever maintains it.
- **`vibe applications`** lists what `GET /v1/applications` answers: the Applications the User contributes to and those shared with them, saying which is which.
- The instruction file of a new project (`AGENTS.md`) says that a deployed Application is visible to its Contributors only until it is shared, names `vibe share`, `vibe unshare`, `vibe contributors add` and `vibe show`, and says to tell the Builder who can see it after a deploy or a share, and to ask the Builder before sharing with everyone.

### What this does not close

- A Platform administrator changing the Contributors of an Application whose Contributors have all left (the ticket on what Platform administrators see lists every Application; taking one over is not built).
- Sharing from the Dashboard, and the list there: the ticket on the Dashboard ("The Dashboard").
- A record of who was removed and when, beyond the log's line: each entry says who added it and when; a removal leaves only the log.
- Disabling and deleting an Application: their ticket.

## A write whose answer is lost (ticket: Review findings: a write whose answer is lost can land after its read-back)

"The program decides from what it holds in memory, which it keeps exactly in step with the platform database it alone writes." This section says what that means when the program cannot tell what became of a write. It holds for everything the program holds in memory: an Application's two lists (already so, "Contributors and sharing: Opening an Application"), **which Version serves**, **a Resource's Grants**, and **the Resources**.

### What can happen

A request of the API that writes one of these and is answered `503` `unavailable` may or may not have been made, and may still be made after that answer: its commit was on its way when the program stopped waiting for the platform database. Nothing new is promised about the answer itself: it stays `503`, and asking again is safe (a Version sent again is `migrations-diverged` or a new Version as before; a go-back asked again is `serving-changed` when the first was made; a Grant set again is the same Grant; a Resource registered again is `name-taken`).

### The rule

From that answer on, **the program never goes on deciding by what it held** while it cannot tell what became of the write. It reads the platform database again only in a way that cannot be earlier than the write (a write that is still on its way is waited out, never read past), and until it has:

| What was being written | Meanwhile | Once it is settled |
|---|---|---|
| Which Version serves (a Version sent, a go-back) | Every request to that Application's address is `503` with `retry-after`, as for lists not known: no Version of it is run on a guess | Requests are served by the Version the database says serves |
| A Resource's Grant | Every operation on that Resource that would come to the check of a Grant is refused `grants-unknown` (the refusals checked before it, `no-request` to `declared-lower`, are given as ever), nothing is sent to the shared database, and its Operation log entry is written like any refusal's; a pull of its structure and `GET /v1/resources/<name>/grants` are `503` `unavailable`; in `GET /v1/resources` and `GET /v1/resources/<name>` the User's `access` to that Resource is not guessed: those answers are `503` `unavailable` too | Operations are decided by the Grants the database has: an operation of a request that arrived after the write was settled is never refused `grants-unknown` ("When the program finds out") |
| A Resource being registered | The program does not know it: operations on it are `not-registered`, as before the request | The program knows it from the next request of the API that names it (registering it again answers `name-taken`, and from that answer on it is there for operations too); never only after a restart |

- "Meanwhile" answers come at once: a request to an Application's address and a request of the API never wait for the platform database to settle, and a request is not held while another reads. An operation is decided at once (its `host-call` record says `refused`, `grants-unknown`); like every refused operation its Operation log entry is written before the Application is answered, and when that entry cannot be written the Application is answered `grants-unknown` all the same, once the program has stopped waiting for the entry ("The Operation log": a refused operation keeps its reason).
- Other Applications and other Resources are not touched: they are served and decided as ever.
- **When the program finds out.** It reads when something needs what it does not know, not before. A request to an Application's address reads, before any of the Application's code runs, what the program does not know of that Application (who may open it, which Version serves) and the Grants it does not know on the Resources its serving Version declares; a request of the API that would show a Resource's Grants, or a User's access to it, reads them. So once the write is settled, the next request to the Application is served by the Version the database says serves, and its operations are decided by the Grants the database has: none of them is refused `grants-unknown`. An operation refused `grants-unknown` makes the program read as well, once it is answered, so that a request already running when the write is settled is not refused to its end. These readings are bounded: at most four are made at once in the program, and those of one request take at most 1 second together; a request that comes while four are being made, or whose readings are not done in that time, gets the "meanwhile" answers.
- `grants-unknown` joins the reasons of "Refusal reasons"; its `fix` says that the platform is finding out the Resource's Grants after a change whose answer was lost, and to try again shortly. The SDK's `Refusal` type has it: `{ readonly reason: "grants-unknown"; readonly resource: string }`, so Application code can tell it apart as it does `no-grant`. The template's `AGENTS.md` names `grants-unknown` and `log-unavailable` beside the other reasons and says of both that nothing in the project is wrong (the platform could not decide or record the operation just then, and the same request works again shortly), so there is nothing to fix in the code and nothing to code around.
- **The same at a start**: the program does not serve an Application by a serving Version, or decide operations by Grants, read while a write of them was still on its way (from the program that ran before). It starts all the same; what was on its way is "meanwhile" above until settled.
- Before the `503` of the write itself is answered, nothing has changed: the database does not hold the write yet, and the program decides by what it held, which is still right.

### What this does not close

- The Operation log and the Change history are written by the program and read from the platform database; they are not held in memory and are not part of this.
- Sessions and tool credentials are read from the platform database at each use.

## The Dashboard (ticket: The Dashboard)

The first page an employee sees: the Applications they contribute to and those shared with them, what each one is, what of it will not work for them, and, for a Contributor, the sharing of it. The program serves it at **the Dashboard's address**, `<scheme>://dashboard.<domain>[:<port>]` (written as "Sign-in and sessions: Starting with OIDC" writes an Application's address). It is pages of the platform's own with links and forms and **no script**: it works in a browser that runs none.

### Signing in to it

- With OIDC the Dashboard is signed in to exactly as an Application is ("Sign-in and sessions: The way through sign-in"), with `dashboard` where the Application's name is. Step 1 on the Dashboard's address: a `GET` or `HEAD` whose `Accept` has `text/html` and that has no session there is answered `302` to `<sign-in address>/sign-in?application=dashboard&next=...&state=...`, any other request `401`, `text/plain`. Step 2: from this ticket on the sign-in address takes `dashboard` as it takes a known Application (never its `404`). Step 4 at `<the Dashboard's address>/_vm/session`. Its session cookie has the name and attributes of an Application's and is valid on the Dashboard's address only: an Application's cookie is no session there, and the Dashboard's is none on an Application's address, on the API or on the sign-in address. Renewal, the end of a session and sign-out are those of every Application session: `POST <sign-in address>/sign-out` ends the Dashboard's too.
- With the stand-in: the `x-vm-stand-in-*` headers, as everywhere. With neither: "Not signed in", as everywhere.
- A request from another origin is refused as "Sign-in and sessions: Whose request it is" says, with the Dashboard's address in place of the Application's: no other page, an Application's included, can share an Application in someone's name or read the Dashboard.
- **Opening an Application from the Dashboard does not ask to sign in again.** An Application's link is its address; a browser with a platform session goes through steps 1, 2 and 4 there without the identity provider being asked ("One sign-in for all").
- A request to the Dashboard has a `request` record like any other, with `application` and `version` null.

### What it answers

| Request | Answer |
|---|---|
| `GET /` | `200`, the list (below); `400` for a `page` that is not a whole number from 1 to 10000 |
| `GET /applications/<name>` | `200`, one Application (below) |
| `POST /applications/<name>/sharing` | a change of whom it is shared with (below) |
| One of these paths with another method | `405`, with `allow`: `GET, HEAD` for the first two, `POST` for the third |
| Any other path | `404` |

- `HEAD` is answered as `GET`, without a body.
- Every answer to someone signed in is a page of the platform's own, the refusals included: `text/html; charset=utf-8`, with the headers every such page has ("Contributors and sharing: Opening an Application": `cache-control: no-store`, `x-content-type-options: nosniff`, `referrer-policy: same-origin`, a `content-security-policy` with `default-src 'none'` and `frame-ancestors 'none'`), and a policy that lets a form lead only to the Dashboard itself (`form-action 'self'`). No page has a `<script>`, an event-handler attribute or a `javascript:` address.
- **Everything on a page that is not the platform's own text is escaped as HTML**, in text and in attributes alike: names, descriptions, Groups, user ids, people's names and emails, a Resource's contact. A description of `<script>alert(1)</script>` is shown as those characters.
- **Every page of the platform's own that the program serves has `referrer-policy: same-origin`** from this ticket on: the sign-in address's pages, the pages an Application's address answers in the platform's name, and the Dashboard's. No address of the platform's is told to another origin (a link to an Application or to the identity provider carries no `Referer`), and a form of such a page is sent with the page's real origin by every browser, also one that sends no `Sec-Fetch-Site` ("Sign-in and sessions: Whose request it is"). The page the tool itself serves on the loopback address while it signs in has no form and keeps `no-referrer`.
- **A page is never wider than its window**, down to a phone's 320 CSS pixels: a name, a description, an address or a contact with no place to break (an Application's name may be 63 characters, a description may hold one long address) is broken where the line ends, never laid out past the edge. Each page declares `<meta name="viewport" content="width=device-width, initial-scale=1">`. No test of the gate can see a layout (none runs a browser): this is checked by hand in a real browser when the pages change.
- Every page says whom the person is signed in as: an element with `data-vm-user="<user id>"`, whose text has their name when the platform knows it. With OIDC it has a link to `<sign-in address>/sign-out`; with the stand-in none.
- While the platform database cannot be reached: `503` with `retry-after`. With the stand-in it is a page saying to try again shortly. With OIDC the session itself cannot be read then, so the program cannot tell who asks, and the answer is the one every address gives someone it cannot tell: `503`, `text/plain`, with `retry-after`, never a redirect to sign in again.

Pages are read by people; the attributes named `data-vm-...` below are what a test (and a Coding agent) reads. The words beside them say the same.

### The list (`GET /`)

- **Exactly the Applications `GET /v1/applications` answers that User**: those they are a Contributor of, then those they can open besides, each group by name (byte order, as the API's). So the Dashboard and `vibe applications` show the same Applications to the same person, and say the same of each which of the two it is. An Application with no serving Version is listed like any other.
- 100 Applications to a page. `?page=<n>` is the n-th page of that order (from 1; without it, the first). When there are more, the page has a link `a[data-vm-next]` to the next one, and from the second on a link `a[data-vm-previous]`. A page past the last is `200` with no Application. A page with no Application says so in words: on the first, that nothing is shared with the person yet and that an Application is made with the `vm` tool.
- Each Application is one element with `data-vm-application="<name>"`, `data-vm-relation="contributor"` or `"shared"`, and `data-vm-serving="<the serving Version's number>"` or `"none"`. Inside it:
  - its name, and its description when it has one, in an element with `data-vm-description`;
  - when a Version serves: a link `a[data-vm-open]` whose `href` is the Application's address in full, with the path `/` (`<scheme>://<name>.<domain>[:<port>]/`). When none serves: no such link, and words saying that nothing is deployed yet;
  - a link `a[data-vm-details]` to `/applications/<name>`;
  - its Contributors, in the order of the list, the first 20: each an element with `data-vm-contributor="user"` or `"group"` and `data-vm-id="<user id or Group>"`, whose text is the person's name when the platform knows it (and their id), or the Group. When there are more than 20: an element with `data-vm-more-contributors="<how many more>"`;
  - **the Resources its serving Version declares**, by name: each an element with `data-vm-resource="<name>"`, `data-vm-declared="<the declared level>"`, `data-vm-yours="structure" | "read" | "write" | "none" | "unknown"` (what `your_access` of `GET /v1/resources/<name>` says for that User; `none` for no Grant and for a Resource that is not registered; `unknown` while the program cannot tell, "A write whose answer is lost": the page reads as `GET /v1/resources/<name>` does there, within the same bound, and is answered `200` all the same, never held until the write is settled), and `data-vm-standing`:

    | `data-vm-standing` | When | What the words say |
    |---|---|---|
    | `enough` | The User holds at least the declared level | nothing more |
    | `lower` | The User holds a Grant below the declared level | what they hold, that what needs more will be refused for them, and whom to ask |
    | `none` | The User holds no Grant on it | that this part of the Application will be refused for them, and whom to ask |
    | `not-registered` | No Resource of that name is registered | that no such Resource is registered, so this part works for nobody yet |
    | `unknown` | The program cannot tell just now | that it cannot be told just now |

    For `lower` and `none` the element has `data-vm-ask="<the Resource's contact>"` and its text shows that contact: **whom to ask**. The mark is in words, not in colour alone. An Application that declares no Resource, or that no Version serves, has no such element.
- Nothing of an Application the person cannot open is on any page of theirs.

### One Application (`GET /applications/<name>`)

- By whoever can open it, and by a Platform administrator, as `GET /v1/applications/<name>`. Anyone else: `403`, a page saying that the Application of that name is not shared with them and to ask one of its Contributors, naming none (as its own address answers them). A name no Application has, or that none could have: `404`.
- The Application as in the list, in an element with the same attributes, with **every** Contributor (no `data-vm-more-contributors`), its address, and the same Resources.
- **A Platform administrator who cannot open it** reads the page all the same, and it does not offer what would be refused: the element has `data-vm-relation="none"` and no `a[data-vm-open]` (the address is still there, as text), and words saying that they read it as a Platform administrator and that it is not shared with them. The Resources are marked for them as for anyone.
- **A person the platform has never seen** (it knows neither a name nor an email of them: nobody of that id signed in here, so a mistyped id shows): with OIDC their element, among the Contributors or among whom it is shared with, here and in the list, has the attribute `data-vm-unseen` and words saying that they were not seen signing in here yet and to check the id, as the tool says it. Someone the platform knows a name or an email of has neither. The stand-in keeps no record of people: there nobody has the attribute or the words.
- **To a Contributor and to a Platform administrator: whom it is shared with**, in the order of the list: each an element with `data-vm-shared="user" | "group" | "everyone"` and, for the first two, `data-vm-id`; a person with their name and email when known. When it is shared with nobody, words saying that only its Contributors can open it. To anyone else no such element, and words saying that only its Contributors see whom it is shared with.
- **To a Contributor: the forms.** One `form[data-vm-form="share"]` and, beside each entry of whom it is shared with, one `form[data-vm-form="unshare"]`; each `method="post"` with `action="/applications/<name>/sharing"`. Someone who is not a Contributor gets no form. With `?changed=shared`, `?changed=unshared` or `?changed=nothing` the page has an element with `data-vm-notice` of that value saying so (what a change redirects to); any other `changed` is ignored.

### Sharing from it (`POST /applications/<name>/sharing`)

- The body is `application/x-www-form-urlencoded`, at most 4 KiB, with the fields `action` (`share` or `unshare`), `kind` (`email`, `user`, `group` or `everyone`) and `value` (the email address, user id or Group; surrounding white space dropped; for `everyone` it is not looked at and may be left out). The share form has a `select` or radio buttons named `kind` with those four values and a text input named `value`; each unshare form has its three fields hidden, naming its entry (a person by `user`).
- **It is the change `POST /v1/applications/<name>/sharing` makes** with that one entry in `add` (`share`) or in `remove` (`unshare`): by a Contributor, with the same rules, the same refusals and the same `sharing-changed` record. It takes effect as that change does: the person shared with finds the Application on their Dashboard and opens it at their next request, and after an unshare neither.
- Made: `303` to `/applications/<name>?changed=shared` or `?changed=unshared`, or `?changed=nothing` when everything was so already.
- Refused: a page with the status the API gives and an element with `data-vm-error="<the API's reason>"`, and a link `a[data-vm-back]` to `/applications/<name>`; nothing is changed. The element's text is for a person at a browser, not a Coding agent: it says what was refused and what to do, in the Dashboard's own words (no path of the API, no command's JSON). It names what the refusal is about: the Application's name for `not-found`; the address as given for `unknown-email` (saying that nobody who signed in here has it, and to use the person's user id or ask them to sign in once) and for `ambiguous-email` (to use the user id); the bound of 1,000 for `too-many`; for `not-a-contributor` it says that only the Application's Contributors change whom it is shared with, and names none of them; for `malformed` what was wrong with the form. `404` `not-found`; `403` `not-a-contributor` (also for someone who cannot open it, as on the API); `400` `malformed` (another content type, a body over 4 KiB, a field missing or given twice, an `action` or `kind` that is none of those, a `value` the API would not take); `422` `unknown-email`; `409` `ambiguous-email`; `409` `too-many`; `503` `unavailable` (with `retry-after`; also while the platform database cannot be reached: a change's `503` always has this element); `408` `timed-out` when the form's body has not come whole within 10 seconds.
- A Contributor cannot take themselves off the Contributors here, and nobody is made a Contributor here: the Contributors are changed with the tool.

### What this does not close

- Changing the Contributors, creating, describing, disabling or deleting an Application from the Dashboard.
- Searching the list.
- What a Platform administrator sees of every Application: its ticket. Here an administrator's list is their own, as anyone's.
- A Dashboard in another language than English.

## The local run on a busy machine (ticket: The local run's private Postgres passes its time limits under the gate's load, and the refusal does not say which limit)

A local run that ends `postgres-failed` (exit code `1`; "The local loop: one request as a test User") says **what happened to which step**, so that a failure on a busy machine is read from the command's own output and not guessed.

### What `postgres-failed` says

- The message keeps what it has (the step in words, `.vibe/local/postgres.log`, what to run next) and says the cause in words: that the step did not end within its limit, with the limit in seconds; or that the connection to the private Postgres could not be opened or was lost; or that the server refused, with its SQLSTATE; or that the private Postgres did not start or ended.
- With `--json` the `error` object has, beside `reason` and `message`:
  - `"step"`: a short fixed name of what the run was doing, the same for the same step in every run (lower-case words joined by `-`, for instance `own-database-create`, `own-database-connect`, `stand-in-create`, `start`; the program's own list, each documented by its message);
  - `"cause"`: `"timed-out"`, `"connection"`, `"server"` or `"ended"`;
  - `"limit_seconds"`: a number, with `timed-out` only;
  - `"sqlstate"`: five characters, with `server` only.
  A member that does not apply is left out, not `null`. When a step was tried more than once before the run gave up (a connection opened again and again), `limit_seconds` is the whole time the run gave the step, and the words say that time, not one try's.
- Nothing of a password or a connection string is in any of them.
- The exit code, the reason, and that nothing the run started is left running are unchanged.

- Only a failure of the private Postgres is `postgres-failed`. A `.vibe/local/` that cannot be written (its `state.json` included) is `local-state`, as before, with the same words (removing `.vibe/local/` starts the local databases anew): so is a `.vibe/local/postgres.log` that cannot be written (for instance, it is a folder), and a data folder `.vibe/local/postgres/` that holds no Postgres data yet and cannot be made anew (for instance, it is a file). Neither has a `step` or a `cause`; something of the program's own that is not Postgres's (no randomness for a password, a local platform database written by a newer program) is `failed`, with what to do.

### The local databases do not wait on the disk

Measured for this ticket (the numbers are in the ticket): with 34 local runs starting at once on the gate's machine, making the own database took up to 18.8 s of its 20 s limit and stopping the private Postgres a median of 15 s, all of it waiting on `fsync` (checkpoints of 813 files after `initdb`, the journal's commits under `CREATE DATABASE`), none on the processor; with `fsync` off the same steps took 0.24 s and 0.1 s.

- **The private Postgres of the local run runs with `fsync` off and `full_page_writes` off.** Everything in it can be made again: the stand-ins from the pulled structures and the project's test rows, the own database from the migrations and `test-rows/own-database.sql`, the local records by running again.
- What this costs, and what the Builder is told: the local databases are **not kept through a crash of the machine or a loss of power** (a kill or a crash of `vibe` or of Postgres loses nothing that was answered: a commit is with the operating system before its answer is given, so the private Postgres no longer runs with `synchronous_commit` off, which let an answered commit be lost to a kill in the moment after; with `fsync` off that costs a write and no wait on the disk). After such a crash a local run may end `postgres-failed`; `vibe local reset` starts the local databases anew and loads the test rows again. Rows that must be there after that belong in the project's `test-rows/` files. The project template's `AGENTS.md` says so where it describes `vibe local reset`.
- A private Postgres made by an earlier program is started with the same settings: nothing is to be migrated.
- The time limits of the steps stay as they are (they bound a step that hangs; they are no longer what a busy disk runs into).
- This is the local run only. An installation's platform database and the own databases on it are IT's Postgres and are not touched.

### The harness

- A program the harness writes for a test to start (a stand-in `node`, a stand-in tool) is never started while it is still open for writing in any thread's child: "Text file busy" is the harness's own race, not the program's, and no test may fail by it.

## The command's name (ticket: The tool's command is `vibe`)

The program is `vibe`: one binary, built as the package `vibe` (`cargo build -p vibe --bin vibe`, `target/debug/vibe`), and every name that follows the command follows it:

- the command in the usage text, in every message that says what to run, in the project template (`AGENTS.md`, `package.json`'s scripts, `gitignore`) and in the first line of a generated file ("Generated by vibe pull.");
- the project's folder `.vibe/` (`.vibe/local/`, `.vibe/packages/`), the tool's log `.vibe/local/vibe.log`, the project's file `vibe.dev.json`;
- the tool's folders: `vibe/` in the user's configuration folder and in the user's data folder, on each system where `vm/` was;
- every environment variable the program reads, `VIBE_...` where it was `VM_...`, and the harness's own (`VIBE_BIN`, `VIBE_TEST_PG_BIN`).
- what the program calls itself to Postgres: the `application_name` of its connections (`vibe`, and `vibe relay` for the relay's) and the comment it puts on an own database (`vibe application <name>`).

The platform's own names inside are not the command's and stay: the schema `vm` of an own database and everything in it, the settings `vm.ctx` and `vm.actor`, the marks `data-vm-...` of the platform's pages, the headers `x-vm-...`, the cookies' names, the paths under `/_vm/` that the platform serves on an Application's address (`/_vm/test-user`), the error codes `VM...`, the SDK's package `@vm/sdk`, the bytes a check code is derived from.

Nothing is kept for the old name (nothing was installed anywhere):

- No output of the program, to the terminal, in a page, in a log record or in a file it writes, tells anyone to run `vm`, or names `.vm/`, `vm.dev.json`, `vm.log` or a variable `VM_...`.
- A project that has a `.vm/` folder and a `vm.dev.json` from before is a project without local state and without a file of test Users: neither is read, moved or removed; the run makes `.vibe/` beside them.
- A `VM_...` variable in the environment is not read: with `VM_LOCAL_POSTGRES_BIN` naming a folder that does not exist and no `VIBE_LOCAL_POSTGRES_BIN`, a local run goes as if neither were set; `serve` with `VM_DATABASE_URL` set and no `VIBE_DATABASE_URL` ends as it does with no address at all.

## One Postgres version (ticket: One Postgres version: 18)

The platform supports **Postgres 18** and no other major version: the installation's Postgres, the local run's private Postgres, and the harness's.

- **An installation.** `vibe serve` and `vibe setup` look at the server's version when they first connect, before anything is written to it. A server whose major version is not 18 is a start-up failure as "Serving" says (exit code not `0`, within 15 seconds, a message on standard error): the message says the version the server has (as the server gives it, for instance `17.6`) and that Postgres 18 is needed. Nothing was written: no platform database was made, and in a platform database that was there no table, row, role or database was made or changed. The message is for a person and is on standard error; beside it `vibe serve` writes the log line `{"event":"postgres-version-unsupported","found":"<version>","needed":"18"}` (`vibe setup` writes no such line: of this refusal the message is all it says, and it prints no credential).
- **The local run.** The private Postgres is the 18 the tool downloads. A `VIBE_LOCAL_POSTGRES_BIN` that names a Postgres of another major version ends `postgres-unavailable` as before, and the message says the version found, that 18 is needed, and what to do: unset `VIBE_LOCAL_POSTGRES_BIN` and the tool downloads one.
- A minor version is never looked at: any 18.

## The tool reaches an installation over HTTPS (ticket: The tool reaches an installation over HTTPS, and a company's own certificate authority is trusted)

Until this ticket the tool spoke plain HTTP to a loopback address only, so it could not reach an installation on another machine. From here on HTTPS is the way, and plain HTTP stays what it was.

### Where the tool connects

```
vibe sign-in --domain <domain> [--port <n>] [--resolve <ip>] [--no-browser] [--timeout <seconds>] [--json]
vibe sign-in --domain <domain> --connect <ip:port> ...                      (plain HTTP, as before)
VIBE_SETUP_CREDENTIAL=... VIBE_OIDC_CLIENT_SECRET=... \
vibe admin connect-oidc --domain <domain> [--port <n>] [--resolve <ip>] | --connect <ip:port>  ...
```

- **Without `--connect`** the tool speaks HTTPS (TLS 1.2 or later) to `api.<domain>`, on port `443` or the one `--port` gives (1 to 65535; anything else is exit code `2`). The name is resolved by the system; `--resolve <ip>` (an IPv4 or IPv6 address, written alone; anything else is exit code `2`) connects to that address instead and changes nothing else: the TLS server name is `api.<domain>`, the certificate is checked for that name, and `Host` is `api.<domain>` (with `:<port>` when the port is not 443). It is for a name that is not in DNS yet, and for tests.
- **With `--connect <ip:port>`** everything is as it was: plain HTTP, a loopback address only (another address is exit code `2`), `Host: api.<domain>`. `--connect` together with `--port` or `--resolve` is exit code `2`. No option makes the tool speak plain HTTP to another machine, and none turns the certificate's check off.
- **The certificate** must be valid now, for the name `api.<domain>`, and issued under an authority the program trusts: the public ones built into it, and those of `VIBE_CA_FILE` ("A company's own certificate authority"). When it is not, the command ends with reason `certificate`, exit code `1`, before any request is sent (no credential, no setup credential, no verifier left the machine), and the message says which it was: the certificate is for another name (it says the name asked for), its authority is not known (it names `VIBE_CA_FILE`), or it is expired or not valid yet; any other failure of the handshake is the same reason with the handshake's own words.
- An installation that cannot be reached (the name does not resolve, nothing listens, no answer in time) ends as it does with `--connect`: the same reason and exit code, the message naming `api.<domain>` and the port. Connecting and the handshake together have the connection's time limit, as before; every wait has one.
- **The credential file keeps how the installation is reached** (HTTPS with its port and, when given, the address of `--resolve`; or the `--connect` address), so every later command of the tool (`vibe whoami`, `vibe applications`, `vibe deploy`, `vibe logs --follow`, `vibe sign-out`, `vibe admin connect-oidc` with the tool's own credential, ...) reaches it the same way with no option. A credential file written before this ticket (it has a `--connect` address) is read as before. The command the `sign-in-expired` message gives has the same options the sign-in was made with.
- The tool follows no redirect, as before, and sends its credential to `api.<domain>` over the checked connection only.
- Signing in through the browser is unchanged: the tool opens the sign-in address the installation gives it, and listens on `127.0.0.1` for the browser's return.

### A company's own certificate authority

- `VIBE_CA_FILE=<path>`, in the environment of any command of the program, names a file of certificates in PEM form (one or more; at most 1 MiB). They are trusted **beside** the public authorities built into the program, for every HTTPS connection the program itself makes: the tool to the installation, the platform to the identity provider (its configuration, its keys, its token endpoint), and the local run's download of Postgres. Unset or empty: the built-in ones alone.
- The file is read when the command starts (`vibe serve`: at start, before it listens; a changed file counts from the next start). A file that is not there, cannot be read, is larger than 1 MiB, holds no certificate, or holds one that cannot be read or used (every `CERTIFICATE` section of the file must be a certificate the program can take as an authority: one that is not fails the whole file, it is not skipped; sections of another kind are passed over): `vibe serve` and `vibe setup` fail at start as "Serving" says, the message naming `VIBE_CA_FILE` and the path; a command of the tool that is about to make an HTTPS connection ends with reason `ca-file`, exit code `1`, before anything is sent (a command that makes none, such as `vibe new` or one that reaches its installation with `--connect`, does not look at the file). Never a silent fall back to the built-in ones.
- It is not a secret (certificates are public) and may be named in a message; no key is ever read from it, and a private key in the file is not a certificate: a file with nothing else is the failure above.
- **The identity provider.** With the provider's certificate issued under an authority of the file, `PUT /v1/oidc` and every sign-in work as with a public one. Without the file (or with a file that does not hold that authority), `PUT /v1/oidc` is `422` `identity-provider-unusable` as before, and the message says that the provider's certificate was not trusted and names `VIBE_CA_FILE`; a certificate that is for another name than the issuer's, expired or not valid yet is the same refusal whatever the file holds, and the message says which of these it was; a connection that is in the platform database is kept and its sign-ins end as with a provider that cannot be reached.
- **The local run's download of Postgres** ("The local run": `VIBE_LOCAL_POSTGRES_MIRROR`). From an `https` mirror whose certificate is issued under an authority of the file, the download works as from a public one. Without that authority it is `postgres-unavailable` as any failed download, and when the certificate's authority is the one not known the message names `VIBE_CA_FILE`; a certificate that is for another name than the mirror's, expired or not valid yet is `postgres-unavailable` whatever the file holds, the message saying which of these it was, and nothing is asked of the mirror. A file that cannot be used is reason `ca-file`, exit code `1`, found when the download is about to make an HTTPS connection and before anything is asked over it: with an `https` mirror (its scheme in any letter case) that is before anything is asked at all; with an `http` mirror that redirects to `https`, it is at the redirect. A download that never leaves `http` does not look at the file.
- Nothing the program serves changes by it: it gives no certificate to anyone and trusts nothing more on what it listens on.

## Installing: one image and a Compose file (ticket: Install: one image and a Compose file, hardened)

Installing is one container image and one Compose file beside a Postgres 18 (ADR 0015). What is said here is what an outsider with Docker can observe; it is tested by the install tests (`blackbox/tests/install_*.rs`), which are not part of the gate: they need Docker and a built image, are marked ignored with that reason, and are run by `scripts/install-gate.sh`, which builds the image from the working tree first and gives its name to the tests in `VIBE_INSTALL_IMAGE`. The release workflow runs the same script before it publishes anything.

### The image

- Built by `docker build .` from the `Dockerfile` at the repository's root, with nothing else on the machine than Docker (the build stage brings the toolchain of `rust-toolchain.toml` and what the build needs; the lock file is honoured: a build whose lock file would change fails).
- It holds the program `vibe` on the path and runs it: `docker run --rm <image> --version` prints what `vibe --version` prints (its first line, the name and the version, is the same as the tool's of the same source; the lines after it describe the build). Its default command is `serve`; what `serve` needs comes from the environment and the command line as "Serving" says.
- It runs as a user that is not root, given as a number (so that `runAsNonRoot` can check it), and needs nothing writable: it starts and serves with a read-only root filesystem, every capability dropped, `no-new-privileges`, no volume and no tmpfs. The program writes its log to standard error and nothing to disk.
- It needs no certificate authorities of the system: the public ones are built into the program. A company's own authority is a file mounted read-only and named by `VIBE_CA_FILE` ("A company's own certificate authority").
- It holds `/usr/share/doc/vibe/THIRD-PARTY-LICENSES`: every crate of the lock file that is built into the program for the image's target, with its version, its licence as its manifest states it, and the licence texts its source carries (a crate whose source carries none is listed with its manifest's statement and that fact). It is made at the image's build from the lock file, never kept by hand. It is plain text: each crate begins with a line `## <name> <version>`, the next line is `License: <its manifest's statement>`, and its texts follow, each line of a text indented by four spaces, so that no line of a text stands at the margin as a `## ` line does (a licence's own text may have such lines: a `## ` line begins an entry only when the line after it begins `License: `); the workspace's own crates are not in it. After the crates comes a part for **the code built into the program that is not a crate's own** and whose licence texts no crate's source carries: V8, the libraries linked into V8's prebuilt build each as an entry of its own (ICU, the C++ library libc++, abseil, and the smaller ones: whatever of them has code in the program), and Postgres's parser (libpg_query, the PostgreSQL source it is made of, and what libpg_query vendors). The Rust standard library and the crates it brings that the lock file does not name are in this part too, built in through `rustc`. Each begins with a line `## <name> <version> (built in through <crate>)` (the version is the commit where the project gives that build no other), the next line is `License: <its SPDX name>`, then a line `Taken from: <the address and the tag or commit>`, and its texts follow. Those texts are kept in the repository under `deploy/third-party-licenses/`, each fetched from its project's own repository at the version that is built in, never written by hand. The licences of what the image's base brings (the C library and the like) are the base's own files under `/usr/share/doc/`, left as they are. The program's own `LICENSE` comes with the ticket of the first release.
- It holds no compiler, no package manager, no source, and no shell is needed to run it (there is none).
- Its labels say where it comes from: `org.opencontainers.image.source` (the repository), `org.opencontainers.image.version` (the version of the first line of `vibe --version`) and `org.opencontainers.image.licenses`.

### The Compose files

`deploy/compose.yaml` works as downloaded, alone (a folder holding it, the `.env` and `certs/` with the two certificate files, and nothing else: no `ca/` folder, no other file), with a `.env` file beside it holding the values; `deploy/.env.example` lists every value with how to make it, and `deploy/README.md` is the install document (from a machine with Docker to a signed-in person, every command written out).

| Value | What it is | Without it |
|---|---|---|
| `VIBE_DOMAIN` | the installation's domain; Applications are at `<name>.<domain>` | Compose refuses, naming it |
| `VIBE_SECRET_KEY` | the installation's key ("The installation's key"): `openssl rand -base64 32` | Compose refuses, naming it |
| `POSTGRES_PASSWORD` | the bundled Postgres's password: `openssl rand -hex 24` | the bundled Postgres does not start: `docker compose up` ends with an error, `vibe` is not started, and the `postgres` service's log names `POSTGRES_PASSWORD`. Taken away from a Postgres already made, that Postgres runs and is never healthy (its health check's own output, which `docker inspect` shows, says that `POSTGRES_PASSWORD` is empty), so `vibe` is not started with an empty password either. Not needed with `compose.own-postgres.yaml`, and never asked for then |
| `VIBE_BIND` | the address the installation is published on | `127.0.0.1`: reached from this machine only |
| `VIBE_HTTPS_PORT` | the port browsers and the tool reach | `443` |
| `VIBE_IMAGE` | the image | the published image of this release |
| `VIBE_DATABASE_URL` | with `compose.own-postgres.yaml`: the administrator's Postgres 18 and the platform database in it | asked by that file only; Compose refuses then, naming it |
| `VIBE_HTTP_BIND`, `VIBE_HTTP_PORT` | with `compose.own-proxy.yaml`: where the program's own port is published for the administrator's proxy | `127.0.0.1`, `8080` |
| `VIBE_CA_FILE` | a file of `deploy/ca/`, as the container sees it (`/etc/vibe/ca/<file>`): the company's own authority | none: the built-in public authorities alone |

- **The same on every Compose version.** Compose versions differ in whether a value named inside another value's default is looked at when the outer value is set (2.38.2 looks at it always, 5.0.2 only when the outer one is missing; found by the release workflow's first rehearsal). The files do not depend on it: a value Compose is to ask for (`${NAME:?...}`) is never written inside another value's default. This is why Compose itself asks for `VIBE_DOMAIN`, `VIBE_SECRET_KEY` and, with `compose.own-postgres.yaml`, `VIBE_DATABASE_URL`, and the bundled Postgres itself for `POSTGRES_PASSWORD`. The install tests run on two versions of Docker, the machine's (the install gate) and the hosted runner's (the release workflow); `deploy/README.md` names the Engine and Compose versions the release's install tests ran with, and says of the minimum it gives that it is read from Docker's notes and was not run.
- **Services.** `vibe` (the image), `postgres` (the bundled Postgres 18, its data in a named volume, published nowhere), and `proxy` (a reverse proxy that terminates TLS with the administrator's wildcard certificate for `*.<domain>`, read from `deploy/certs/fullchain.pem` and `deploy/certs/privkey.pem`, and passes every request on with its `Host` unchanged and no limit of its own below the platform's on a request's size or time). Each image is named with its version, never `latest`.
- **The bundled Postgres** holds the role `vibe`, which may create databases and roles and is not a superuser, with the password `POSTGRES_PASSWORD`, and the platform database `vibe`; the platform connects as that role and never as a superuser. The superuser `postgres` cannot connect over the network at all, whatever password is given, its own included (the administrator reaches it inside the container: `docker compose exec postgres psql -U postgres`), and `CONNECT` on the databases `postgres` and `template1` is revoked from `PUBLIC`, which a platform that is no superuser cannot do itself ("What this does not close" of the own databases). `deploy/README.md` says the same two things to do on a Postgres the administrator brings. All of this is made at the bundled Postgres's first start, and the `postgres` service is healthy only once that first start has finished whole: one that was interrupted (the container stopped or killed while Postgres was being made) leaves the service unhealthy at every later start, so `vibe` is not started on a Postgres that was half made, and `deploy/README.md` says what to do then (`docker compose down --volumes` and start again: nothing is in it yet).
- **`vibe` as started by the file**: read-only root filesystem, all capabilities dropped, `no-new-privileges`, the image's non-root user, no tmpfs, and one mount: the folder `deploy/ca/` read-only at `/etc/vibe/ca` (empty as downloaded). `serve --domain <VIBE_DOMAIN> --listen 0.0.0.0:8080 --public-scheme https --public-port <VIBE_HTTPS_PORT>`; never `--stand-in-sign-in`. It is restarted unless stopped. The installation's key and the database's address are in its environment and in no command line.
- **Listening.** With the file as downloaded only `VIBE_BIND:VIBE_HTTPS_PORT` is published, on loopback; `VIBE_BIND=0.0.0.0` is the explicit setting that lets other machines in. The program's own port and Postgres's are published nowhere.
- **Behind the administrator's own reverse proxy** instead of the file's: `docker compose -f compose.yaml -f compose.own-proxy.yaml up -d`. There is no `proxy` service then, and the program's own port is published at `VIBE_HTTP_BIND:VIBE_HTTP_PORT` (`127.0.0.1` and `8080` without them), plain HTTP, for that proxy only. That proxy must pass `Host` on unchanged; the addresses the platform writes come from `--public-scheme` and `--public-port`, never from a header of the request.
- **With the administrator's Postgres**: `VIBE_DATABASE_URL` in `.env` and `docker compose -f compose.yaml -f compose.own-postgres.yaml up -d`. There is no `postgres` service and no volume then; without `VIBE_DATABASE_URL` Compose refuses, naming it. The role must be allowed to create databases and roles; a server that is not Postgres 18 is refused as "One Postgres version" says, and the container's log says so. The two other files can be given together.
- **The setup credential** is printed once by `docker compose exec vibe vibe setup`, to the administrator's terminal and to no log. From there on the steps are those of "Connecting OIDC": the redirect address to register at the identity provider is `https://auth.<domain>[:<port>]/callback`, then `vibe admin connect-oidc --domain <domain> ...` with the tool, from any machine that reaches the installation over HTTPS.
- **Upgrading** is a newer `VIBE_IMAGE` and `docker compose pull && docker compose up -d`: the platform database, every own database, the OIDC connection and the sessions are kept (the data is the volume's, the key the `.env`'s). `docker compose down` keeps them too; only `down --volumes` removes the bundled Postgres's data.

### What the install tests show

Against the image `VIBE_INSTALL_IMAGE` names, started by the files of `deploy/` themselves, copied unchanged to a folder of the test's with its `.env`, certificates and authority (a test may add one more `-f` file for what stands in for the outside world, which is name resolution to the identity provider; that file changes nothing else of `vibe`, `postgres` or `proxy`):

- with the file as downloaded and the three required values, the installation comes up; `https://api.<domain>/v1/installation` answers through the proxy with the wildcard certificate, and says no sign-in is connected;
- `docker compose exec vibe vibe setup` prints a setup credential; OIDC is connected with it by the tool outside the container, over HTTPS (`--port`, `--resolve`, `VIBE_CA_FILE` for the proxy's test authority), against an identity provider the container reaches over HTTPS under a private authority given by `VIBE_CA_FILE` in `deploy/ca/`; a person signs in, a Contributor deploys the template's Application with the tool, and a request to `https://<application>.<domain>/` as that person is answered by it (the walking skeleton's request, as installed); sign-out works by its form; every redirect names the public address (`https`, the public port);
- what `docker inspect` says of the running `vibe` container: read-only root filesystem, no capability, `no-new-privileges`, a user that is not root, no mount but `/etc/vibe/ca` read-only; and nothing but the proxy is published, on loopback;
- in the bundled Postgres the role `vibe` is no superuser and may create databases and roles; `PUBLIC` may not connect to `postgres` or `template1`; the superuser is refused over the network with its right password and let in inside the container; after the deploy, the Application's own database and roles are there;
- a folder with nothing but `compose.yaml`, `.env` and `certs/` comes up and answers; a bundled Postgres whose first start was interrupted before its end stays unhealthy when started again and `vibe` is not started; `vibe logs --follow` through the file's proxy receives an entry made after it started;
- restarting `vibe`, and `down` then `up`, keep the Application and the connection and let the same person in again; with `VIBE_IMAGE` naming a second tag of the image (an upgrade), the same;
- with `compose.own-postgres.yaml` and `VIBE_DATABASE_URL` naming a Postgres 18 outside the files, the installation comes up and takes a sign-in's first step; naming a Postgres 17, the `vibe` container's log has `postgres-version-unsupported` and nothing is served;
- a request body larger than the platform's own limit, sent through the file's proxy, is answered by the platform (`413` `too-large`), not cut short by the proxy;
- with `compose.own-proxy.yaml`, behind another reverse proxy that terminates TLS, the installation answers and its redirects name the public address;
- without `VIBE_DOMAIN` or `VIBE_SECRET_KEY`, `docker compose config` fails naming the missing value; with `compose.own-postgres.yaml`, without `VIBE_DATABASE_URL` it fails naming that one, and with it `config` passes with no `POSTGRES_PASSWORD` anywhere; no file of `deploy/` has a `${NAME:?...}` inside another value's default (read from the files, in the gate);
- with `compose.yaml` and no `POSTGRES_PASSWORD`, `docker compose config` passes, `docker compose up -d` ends other than `0` within three minutes, the `vibe` container was never started, and `docker compose logs postgres` names `POSTGRES_PASSWORD`; with the value taken out of the `.env` of an installation that is up, the `postgres` service started again alone is not healthy and its health check's output names `POSTGRES_PASSWORD` within seconds;
- the image's `THIRD-PARTY-LICENSES` names every crate `cargo tree -p vibe --edges normal,build --prefix none` lists for the image's target with its version (but the workspace's own), and each has a licence stated; its second part names V8, ICU, libc++, abseil, libpg_query and PostgreSQL, each with a licence, where it was taken from and a text; the base's own `/usr/share/doc/*/copyright` files are in the image;
- the image has no shell (`docker run --rm --entrypoint /bin/sh <image>` cannot start) and its user is a number other than `0`.

## The first release (ticket: A first, minimal release: the image, the Compose file, the tool for Linux and macOS)

A release is what an administrator and a Builder take from outside the repository: the image on the registry, and a GitHub release holding the Compose files and the tool for two systems. This says what is in one, how it is made, and what holds in the repository so that it can be made. What the gate can see of it is tested in the gate (`blackbox/tests/release_*.rs`), for the commit that is checked out, whether or not a branch `main` names it (the workflow runs the gate on a tag's commit); what only a hosted run can show is shown by the release workflow's own steps, and a run of it by hand is its rehearsal.

### One version

- The program's version is the version of the package `vibe` (`crates/vibe/Cargo.toml`). The first line of `vibe --version` is `vibe <version>`.
- Everything that names a version names that one: the default of `VIBE_VERSION` in the `Dockerfile`; the image `deploy/compose.yaml` takes without `VIBE_IMAGE`, `ghcr.io/irvingouj/vibe:<version>`, and the same name where `deploy/.env.example` and `deploy/README.md` show it; every address of a release's file in `docs/getting-started.md` (`.../releases/download/v<version>/...`) and every archive's name there; the file of the release's notes, `docs/releases/v<version>.md`, which exists.
- For this ticket the version is `0.1.0`.

### The licence

- The program's own licence is the product owner's to state, in a file `LICENSE` at the repository's root (stated on 2026-11-19: the MIT licence, as the manifests say). Nothing is published without it: `scripts/release-check.sh` given a tag fails while it is missing, saying so. A rehearsal goes on without it, saying that it is missing.
- When the repository has it, it is in the image at `/usr/share/doc/vibe/LICENSE` and in each archive of the tool, unchanged; when it has not, neither holds one.

### What a release holds

For the version `<v>`:

- **The image** `ghcr.io/irvingouj/vibe:<v>`, for `linux/amd64`: the image of "Installing: one image and a Compose file", built by `docker build .` from the tagged commit, the same image the install tests of that run passed against.
- **A GitHub release** named and tagged `v<v>`, whose text is `docs/releases/v<v>.md`, with these files and no other:
  - `compose.yaml`, `compose.own-proxy.yaml`, `compose.own-postgres.yaml`: the files of `deploy/`, unchanged;
  - `env.example`: `deploy/.env.example`, unchanged (under this name because a release cannot hold a file whose name begins with a dot);
  - `vibe-<v>-x86_64-unknown-linux-gnu.tar.gz` and `vibe-<v>-aarch64-apple-darwin.tar.gz`: the tool;
  - `SHA256SUMS`: one line for each of the files above, `<64 hex digits>`, two spaces, the file's name, in the names' order: what `sha256sum` writes, so that `sha256sum --check SHA256SUMS` (Linux) and `shasum -a 256 --check SHA256SUMS` (macOS) pass in a folder holding them all, and `--ignore-missing` checks the ones that were downloaded.
- **An archive of the tool** is a gzip'd tar with one folder, `vibe-<v>-<target>/`, holding exactly: `vibe` (the program built `--release --locked` for that target, executable), `THIRD-PARTY-LICENSES`, and `LICENSE` when the repository has one ("The licence") (the notices in the form "Installing" gives the image's, made for that target from the lock file by the same means as the image's: the crates built into the program for that target, then the part for what is built in and is no crate's own).
- The Linux program asks the system's C library for nothing newer than glibc 2.35 (Ubuntu 22.04, Debian 12 and later). The macOS program is not signed with a developer's identity and not notarised: downloaded with `curl` it runs; downloaded with a browser macOS holds it back until its quarantine mark is removed (`xattr -d com.apple.quarantine vibe`), and the page says so.

### The scripts that make it

So that the gate can test the making without a hosted run, each piece is a script of `scripts/` that the workflow calls and does nothing else for:

- `scripts/third-party-licenses.sh [--target <triple>]` writes the notices to standard output, for the given target (without one: the machine's). The image's build uses it: the notices of the image and of an archive for the same target are the same text.
- `scripts/package-tool.sh --binary <file> --target <triple> --out <folder>` makes `<folder>/vibe-<v>-<target>.tar.gz` as said above, `<v>` being what the binary's `--version` says, and prints the archive's path. It refuses (exit code other than 0, saying why, nothing left in the folder) a file that is not there or does not say `vibe <version>` as its first line, and a `<v>` other than the package's version.
- `scripts/release-files.sh --out <folder>` copies the three Compose files and `env.example` into the folder (which may already hold the archives) and writes `SHA256SUMS` over every file then in it.
- `scripts/try-tool.sh --archive <file>` tries a packed tool as a Builder would, on the machine it was built for: it unpacks the archive in a folder of its own, and the unpacked `vibe` must say `vibe <the package's version>` first; it makes a project (`vibe new`), installs its packages (`npm install`), sends it one request on the machine (`vibe request --as outsider /`, which downloads the local run's Postgres), and runs `vibe check` on it, which must pass. `vibe check` needs a signed-in tool, so the script starts an installation on loopback for it (that Postgres, `vibe serve --stand-in-sign-in`), signs the tool in to it, and ends it. Everything is made in one temporary folder, removed at the end with what was started; the tool's own folders are in it too, so nothing of the user's is read or changed. It refuses (exit code other than 0, saying why, before anything is started) an archive that is not there, one that holds no `vibe-<v>-<target>/vibe`, and one whose `vibe` does not say the package's version.
- `scripts/gate.sh` says at its end which of its steps failed, when it went on past one (`--no-fail-fast`): a line `== gate: npm test failed`, a line `== gate: cargo test failed`, each only when that step failed; its exit code is as before (0 only when every step passed).
- `scripts/gate-hosted.sh` is the gate as a hosted runner runs it. The gate's time limits are sized for the machines it is run on every day (12 cores); a hosted runner has 4, and on it a handful of test files that start many programs at once run past those limits when every test file's tests run side by side (the release workflow's rehearsals: six tests of four files, each of which passes when its file runs alone). So the script runs the whole gate once (`scripts/gate.sh --no-fail-fast`), and then each test file that failed once more, alone and one test at a time (`cargo test --locked <what cargo names for that file> -- --test-threads=1`, taken from cargo's own lines "to rerun pass ..." and "N targets failed"). It ends with 0 only when: the gate's steps before the tests passed, npm's tests passed, and every test file passed either the first time or alone. A test file that fails both times fails it, and so does a gate that failed with no test file named. It prints which files it ran again and how each ended, so that a release's log says what did not pass side by side. Nothing is ever skipped or left out.
- `scripts/release-check.sh [<tag>]` checks what must hold before a release is built, and says each thing that does not: the versions of "One version" agree; `docs/releases/v<version>.md` exists; with a tag given, the tag is `v<version>`, its commit is on `main` (`origin/main` itself or an ancestor of it), and `LICENSE` exists. Exit code 0 only when everything asked holds.

### The workflow

- `.github/workflows/` holds one file, `release.yml`, and no other workflow. It starts on a pushed tag of the form `v<digits>.<digits>.<digits>` and by hand (`workflow_dispatch`), and on nothing else: no `push` to a branch, no `pull_request`, no `pull_request_target`, no `schedule`, no other event.
- A run **publishes** only when it runs for a version tag (started by the tag's push, or by hand with that tag chosen). A run by hand for anything else is a **rehearsal**: it builds and checks everything the same way, keeps the files as the run's artefacts, and publishes nothing: no image pushed, no release made.
- Before anything is published, in the same run, all of this has passed: `scripts/release-check.sh` (with the tag, when there is one); the gate on Linux, as `scripts/gate-hosted.sh` runs it, on a runner where a user may make a network namespace of their own without administrator rights (one test of the local run needs it; where the runner's system forbids it by default, the job allows it first and says so); `scripts/install-gate.sh --no-fail-fast`, whose image is the one then pushed (nothing stops at the first failure, so that one run says everything that fails on the runner); on each of the two systems, the tool built there, packed by `scripts/package-tool.sh`, tried by `scripts/try-tool.sh`, and the page's command for checking a download on that system (`sha256sum --check --ignore-missing SHA256SUMS` on Linux, `shasum -a 256 --check --ignore-missing SHA256SUMS` on macOS) passing for the archive against a `SHA256SUMS` that also names a file that was not downloaded; the Linux program's glibc ceiling.
- Publishing is last and in this order: the image pushed; the release made as a draft with every file, then published. A run that fails part-way is run again by hand for the same tag.
- Its permissions are `contents: read` for the workflow; only the job that publishes has `contents: write` and `packages: write`. It uses no secret but the run's own token. Every action it uses is one of GitHub's own (`actions/...`), named by a full commit id, not by a tag or a branch.

### The page and the notes

- `docs/getting-started.md` takes someone from nothing to an Application opened in the browser, with the published files only: installing with Compose (the release's `compose.yaml` and `env.example`; `deploy/README.md` is where the detail is), registering the installation at the identity provider and connecting OIDC, installing the tool (download, check against `SHA256SUMS`, unpack, put on the path) on Linux and on macOS, signing in, a project from the template, running it locally, deploying it, sharing it. The repository's `README.md` leads to it. Every command of the program it shows is one the program has (`vibe <command> --help` knows the command and each option shown).
- `docs/releases/v0.1.0.md`, the release's notes: what works; what is missing, in a user's words; that this release is **not for untrusted Applications or real company data** until the work on credentials, limits and the runtime's hardening is done; that the Postgres a local run downloads is a third party's build, pinned by its checksum; which systems the tool is built for and what was not tried on them; that the image is for `linux/amd64` only.

### What this does not close

- The npm package, Windows, and the local run's tests on three systems: the ticket "The tool on Linux, macOS and Windows, installed as an npm package". On macOS this release's tool is built and checked as far as "The workflow" says, and no further.
- An image for `linux/arm64`; a signed and notarised macOS program; signatures or provenance for the release's files beyond `SHA256SUMS`.
- The program serving TLS itself: its own ticket.

## A real browser in the gate (ticket: The gate runs a real browser)

The platform's own pages are walked by a real browser in the gate, as a person walks them: the browser loads them, fills and submits their forms and follows their redirects, and sends its own headers and cookies and keeps to its own policies. Nothing here changes what a page is or what the platform answers: every sentence about a page above holds as it is, and from this ticket on a browser holds the platform to it. What a browser shows that was written wrong above is corrected there, through the loop.

### The browser

- It is **Firefox**, without a window (`--headless`), driven by the harness over the protocol Firefox itself speaks (Marionette): nothing is installed but Firefox, 128 or later. The harness depends on no product crate, as ever.
- It is found as: the program `VIBE_TEST_BROWSER` names (an absolute path, or a name on the path), else `firefox` on the path. Its version is asked (`--version`) before it is used. A `VIBE_TEST_BROWSER` that is a relative path (it holds a `/` and does not begin with one) is refused as no browser, by the tests and by `scripts/gate.sh` alike, in the same words, which say that the path must be absolute: the gate and the tests run in different folders, and a relative path would name a different file to each.
- **No browser, no gate.** When none is found, or the one found is older than 128 or is not Firefox, every browser test **fails** (it is never skipped, and never passes without a browser), and the failure says what was looked for and what to do: install Firefox 128 or later, or set `VIBE_TEST_BROWSER`. `scripts/gate.sh` looks for it first, with its other checks before anything is built, and stops there with the same words, so that a gate without a browser ends in seconds.
- Each test's browser is its own: a fresh profile in a folder of the test's, no history, no cookie of another test, no add-on, no update, no page of the browser's own opened, nothing sent to the browser's maker. Names of the installation's domain reach the program under test on this machine and nothing else is asked of the network.
- **Each browser a test starts is ended by it**, with every process it started and its profile's folder removed: when the test passes, when it fails, and when the test process itself is killed. After the gate no browser of it runs and no profile of it is left.
- The release workflow's gate has a browser too: the hosted runner's Firefox, whose version the job prints.

### What it walks

Each as a test of the gate (`blackbox/tests/browser_*.rs`), against the real program with OIDC connected to the harness's identity provider, unless said otherwise. A test finds what it clicks and reads by what the pages are said to hold above (the `data-vm-...` marks, the forms, the words), never by a structure nothing above promises.

1. **Sign-in.** The browser asks for the Dashboard with no session, is sent to the identity provider and back, and is on the Dashboard, which says who is signed in.
2. **Opening an Application from the Dashboard.** A click on an Application's link (`a[data-vm-open]`) shows that Application's page, answered for that person, without the identity provider being asked again.
3. **Sharing and unsharing by the Dashboard's forms.** A Contributor shares the Application with another person by the form of its page; the page then shows it shared with them, and that person's own browser finds it on their Dashboard and opens it. The Contributor takes it back by the form; the page then no longer shows it, and the other person's browser is refused the Application.
4. **A refusal in words.** Sharing with an email address the platform does not know is refused on the page, in words that say why, and nothing is shared.
5. **Sign-out.** By the sign-out page's form; afterwards the Dashboard sends the browser to sign in again, and an Application opened before asks for a sign-in again.
6. **The tool's sign-in.** Both ways of `vibe sign-in`: with `--no-browser`, the address the tool prints is opened, the check code it prints is typed into the page's form, and the tool ends signed in (`vibe whoami` says who); with a browser on the machine (`BROWSER` naming a command of the test's that hands the address to the test's browser), the page's confirmation is submitted and the tool ends signed in. A wrong check code is refused on the page in words and the tool is not signed in.
7. **The local run's test-User page.** With `vibe dev` running in a project made from the template, the browser opens the Application's local address, is shown the page of test Users, chooses one by its form, and is shown the Application's page for that User; choosing another shows it for the other.
8. **No page is wider than a narrow window.** In a window 320 CSS pixels wide (the test first holds the window to that width as the page itself sees it), none of these scrolls sideways (the document is no wider than the window): the Dashboard's list with Applications in it, one Application's page as a Contributor sees it, the sign-out page, the tool's sign-in page, a page of refusal, and the local run's test-User page.

In every one of them the browser's own `Origin`, `Referer` policy, `Sec-Fetch-...` headers and cookies are what the platform gets: no test sets a header.

**Over HTTPS, as an installation is reached.** Every walk of an installation's pages (1 to 6, and the installation's pages of 8) goes over HTTPS: the program is started with `--public-scheme https` and stands behind the harness's TLS front ("The tool reaches an installation over HTTPS"), with a certificate for the installation's names issued by an authority of the test's own. The test's browser trusts that authority (put into its fresh profile by the harness; nothing is trusted on anyone's say-so: the browser does not accept a certificate it cannot check, and a harness test shows it refusing the front when the authority was not given to it). Only so does the browser do what it does at a real installation: it keeps and sends the `__Host-` cookies marked `Secure`, and it sends `Sec-Fetch-Site`, `Sec-Fetch-Mode` and `Sec-Fetch-Dest`, which a browser sends to no plain-HTTP address but the machine's own. A harness test shows the second: a form of an HTTPS page of the installation's names, submitted by the browser, arrives with `Origin` and `Sec-Fetch-Site: same-origin`. The identity provider of these walks is the harness's, as before; whether it stands behind TLS is the harness's choice. The local run (7, and its page of 8) is plain HTTP on a name under `.localhost`, as `vibe dev` serves it.

### What this adds to the gate

- The time the browser tests take is measured and said in the ticket.
- The harness's own small browser (`blackbox/src/browser.rs`) stays for the tests that look at one redirect at a time; it is no longer the only thing that holds a form.

### What this does not close

- Another browser than Firefox (Chromium, Safari), and a browser on Windows or macOS: the pages use nothing one of them lacks, but none of them runs here.
- An Application's own pages: what a Builder writes is theirs to test.

## The program serves TLS itself (ticket: The program serves TLS itself with a supplied wildcard certificate)

For an installation with no reverse proxy before it: `vibe serve` ends TLS itself, with the wildcard certificate and the key the Platform administrator supplies. Without the two values below nothing changes: the program listens in plain HTTP, for a reverse proxy or a test, as before.

### The two files

- `VIBE_TLS_CERT_FILE` and `VIBE_TLS_KEY_FILE`, in the environment of `vibe serve` (as `VIBE_CA_FILE` is; there is no option for them): the certificate file, in PEM, the installation's certificate first and the certificates of its chain after it (one to ten certificates), and its private key, in PEM (`PRIVATE KEY`, `RSA PRIVATE KEY` or `EC PRIVATE KEY`; an RSA key of 2048 bits or more, an ECDSA key on P-256 or P-384, or an Ed25519 key). Each file is at most 1 MiB. An empty value counts as not set.
- **Both or neither.** One set without the other is a start-up failure whose message names the one that is missing.
- With both, the listening address serves **HTTPS only**: TLS 1.3 and TLS 1.2, HTTP/1.1 (the one protocol offered by ALPN). Bytes that are not a TLS handshake (a plain-HTTP request to that port) get no HTTP answer: the connection is closed. No client certificate is asked for. The same certificate is presented whatever server name the client asks for; which Application a request is for comes from its `Host`, as ever.
- A handshake that is not finished within 10 seconds is ended, and the connection closed; a connection that has sent nothing yet is in its handshake. A connection in its handshake counts against the same bounds as any other connection of the program.
- `--public-scheme http` together with the two values is a start-up failure whose message names `--public-scheme` (what is served is HTTPS). The public port is as before: `443` unless `--public-port` says otherwise, whatever port the program listens on.
- The listening line says `"tls": true` or `"tls": false`.

### What is checked at start

Each of these is a start-up failure as "Serving" says (exit code not `0`, within 15 seconds, before the listening line and before any connection is taken), with a message that names the variable and the file's path and says why in words; **no message, log line or error ever shows a byte of the key or of the key's file**:

- a file that cannot be read, is empty, is larger than 1 MiB, or holds no certificate (no key);
- a certificate file with more than ten certificates, or whose first certificate cannot be parsed;
- a key file that holds more than one key, a key that is encrypted (a passphrase is not supported; the message says so), a key of a kind not named above (the message names the kinds that are taken), or a key that is damaged: its PEM section is whole but what it holds cannot be read as a key of its label (the message says the key cannot be read, and does not call it a kind that is not supported when its label is one of the three);
- **a key that is not the key of the first certificate** (the message says the two do not belong together and names both files);
- **a first certificate that does not cover `*.<domain>`**: among its DNS names (subject alternative names; the common name is not looked at) there is none that is exactly `*.<domain>` for the `--domain` given. The message names `*.<domain>` and lists the names the certificate does have (at most ten of them).

A certificate that is expired or not valid yet is **not** a start-up failure: it is served (a browser will say what is wrong with it), and the log says so at each load, below. (Decided by the driving session: an installation that comes back after a restart with an expired certificate should say so in its log and to browsers, not refuse to run and take the tool's `vibe logs` with it.)

### Read again when they change

- The program looks at the two files every 5 seconds. When what they hold has changed and the pair passes every check above, connections accepted from then on get the new certificate: **within 10 seconds of the files' change**. Connections already open go on with the certificate they began with.
- A changed pair that does not pass (the certificate replaced a moment before its key; a file half written; a certificate for another domain) changes nothing: the certificate in use goes on being served, and the log says why once for that content, not every 5 seconds, in the words of the start-up failure it would have been (a certificate that does not cover the domain: the reason names `*.<domain>`). When the files change again, they are looked at again.
- The files are found by their path each time, so a replacement by renaming, by writing in place, or by pointing a symbolic link elsewhere (as an orchestrator's mounted secret does) is seen all the same.
- The log: `{"event":"tls-certificate-loaded","names":[...],"not_before":"<RFC 3339>","not_after":"<RFC 3339>","fingerprint_sha256":"<64 hex digits of the first certificate>","valid_now":<bool>}` at start and at each replacement taken; `{"event":"tls-certificate-refused","reason":"<words>"}` for a changed pair that was not taken. Neither holds anything of the key.

### What is the same as behind a proxy

Everything the platform derives from how it is reached is as with `--public-scheme https` behind a reverse proxy, and the tests that show it for that case are run this way too: the sign-in address and the redirect address (`https://auth.<domain>[:<port>]/callback`); every redirect of the sign-in, of an Application's first request and of the sign-out; the cookies (`__Host-`, `Secure`); the `Origin` a form's request is compared with; the address the tool's sign-in prints. The tool reaches such an installation as "The tool reaches an installation over HTTPS" says (`VIBE_CA_FILE` for a certificate of a company's own authority), and `vibe logs --follow` receives entries as they come. A browser walks it: one of the walks of "A real browser in the gate" (sign-in, the Dashboard, an Application opened from it) is run against the program's own TLS, with no front of the harness between them.

### The Compose file

- `deploy/compose.own-tls.yaml`, given after `compose.yaml` with `-f` as the other two: there is no `proxy` service; `vibe` itself is published at `VIBE_BIND:VIBE_HTTPS_PORT` (`127.0.0.1` and `443` without them) and reads `deploy/certs/fullchain.pem` and `deploy/certs/privkey.pem`, mounted read-only (`/etc/vibe/certs`), named by the two variables. The key must be readable by the image's user: `deploy/README.md` says which number that is and the two commands that give it the key. The file sets nothing of `vibe` but what it publishes, the two variables and that mount (no other command, no other key of the service). Everything else of `vibe` as started by the file stays as "Installing" says: read-only root filesystem, no capability, `no-new-privileges`, not root, no tmpfs; its mounts are then `/etc/vibe/ca` and `/etc/vibe/certs`, both read-only.
- It cannot be given together with `compose.own-proxy.yaml` (each replaces the proxy with something else): `deploy/README.md` says so. With `compose.own-postgres.yaml` it can.
- `deploy/README.md` documents it as the alternative to the file's reverse proxy: what it is for, the command, replacing the certificate (copy the new files over the old ones, key last or both by renaming; the program takes them within 10 seconds, no restart; `docker compose logs vibe` shows `tls-certificate-loaded`), and what it lacks beside the proxy (HTTP/2).
- It is a file of a release from the next version on: `scripts/release-files.sh` writes it beside the two other files and `SHA256SUMS` has its line ("What a release holds": the list grows by it; `v0.1.0`, already published, does not have it, and nothing that names `v0.1.0`'s files changes).
- The install tests show it: with `compose.own-tls.yaml` and the three required values, the installation comes up with no `proxy` container; `https://api.<domain>/v1/installation` is answered by `vibe` itself with the wildcard certificate; OIDC is connected, a person signs in, and the template's Application deployed with the tool answers her at `https://<application>.<domain>/`; `docker inspect` shows the same hardening and the two read-only mounts; the certificate files replaced in `deploy/certs/` are served to a new connection within 10 seconds with no container restarted; without the key readable by the image's user, `vibe` does not start and its log names `VIBE_TLS_KEY_FILE` and says the file cannot be read.

### What this does not close

- HTTP/2 and HTTP/3 from the program itself; a listener on port 80 that redirects to HTTPS; `Strict-Transport-Security`.
- Certificates got by the program itself (ACME); a certificate per Application; client certificates.
- TLS older than 1.2 is not offered (the library built in has none); no test of the gate shows a TLS 1.1 client being refused, since the harness's own client cannot speak it.
