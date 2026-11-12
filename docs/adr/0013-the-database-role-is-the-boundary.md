---
status: accepted
---

# The database role is the boundary, and identity is unforgeable only where the platform owns the database

When an Application reaches a database, what holds the Access level is the Postgres role the host logged in with, not an inspection of the SQL. The host keeps a separate login, and a separate connection pool, for each Access level of each database, and never switches role inside a session. A read is additionally run in a read-only transaction. The host still parses every statement and refuses kinds and functions an Application has no business with, but that check is hygiene: a spike found SQL that passes any such check and still runs other SQL (`docs/research/what-postgres-sees.md`).

For a shared database, the Platform administrator who registers it supplies two logins, one that can read and one that can also write, and optionally a third that can see structure and no rows. The platform installs nothing in a database it does not own.

The User's and the Application's identity is written into the session in a way Application SQL cannot forge only in an Application's own database, where the platform owns the structure and can install what that needs. In a shared database with nothing installed, the identity in the session is a label that Application SQL could overwrite. The Operation log does not depend on it: the host writes the log itself, from what it knows about the request.

Limits on time, rows and size are enforced by the host, because Application SQL can lift any limit set inside Postgres.

## Considered Options

- **One login and a switch of role per operation**: one pool per database. Rejected: Application SQL can switch back, with or without the statement check.
- **The statement check as the boundary**: rejected for the reason above.
- **A Postgres role per User**: identity would be native to Postgres, but it needs a pool per User and the database's owner to create a role for every employee.
- **Requiring the platform's identity functions in every shared database**: gives unforgeable identity everywhere, at the price of asking every database owner to install something before the database can be registered.

## Consequences

- Registering a shared database is cheap for its owner: two or three logins, nothing installed.
- In Milestone 1 a shared database's owner cannot write row-level rules that trust the platform's User identity, and Change history is not available there. Both need the owner to install the platform's small identity schema; this joins Change history on shared databases on the roadmap (ADR 0005).
- All Applications share the same logins on a shared database, so one Application can see and disturb another's sessions there. This is within the assumption that Application code is not hostile (ADR 0002).
- An Application's own database has two roles: a platform role that owns every object and applies migrations, and a runtime role for the Application that owns nothing. This is what keeps the Change history from being switched off by Application SQL.
- Verifying identity in the Change history costs time per changed row; bulk writes need it done once per statement before this is production-ready.
- A structure pull can reveal a little data through view definitions, constraint text and comments.
