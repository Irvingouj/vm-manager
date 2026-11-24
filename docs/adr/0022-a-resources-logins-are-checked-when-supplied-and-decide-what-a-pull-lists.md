---
status: proposed
---

# A Resource's logins are checked when they are supplied, decide what a pull lists, and are replaced or removed without touching what they point at

The spec (user stories 16 to 18) gives a Platform administrator the registration of shared databases and folders, the replacement of their credentials and their removal, and lets them narrow what a Resource exposes "with ordinary Postgres privileges on the logins they supply". The ticket "Resources and Grants in full" builds that in stages; ADR 0021 is its first stage (sealing). This is its second: what an administrator does with a Resource. `blackbox/CONTRACT.md`, "Resources and Grants in full" (from "Who may" to "The instruction file"), is the interface this decision led to.

Status `proposed`: written on 2026-11-23. The choices marked "chosen by the agent" that are about what the product does for its users are put to the product owner in issue #93; the others are technical and stand until somebody finds a reason against them.

## What is decided

**A login is checked when it is supplied, and a login that does not connect is refused.** Registering a database and replacing its logins connect as every login given, the way an operation connects, before anything is stored; one that does not connect is `422` `login-failed`, naming which login and why and nothing else of it. There is no way to store a login that failed (chosen by the agent; #93). A catalog-only probe on the read and the structure login gives warnings, never refusals: a read login that can write, a structure login that can read rows, a read login that reaches no table. The platform still installs nothing in the shared database (ADR 0013).

**What the Resource's read and write logins reach is what a pull lists** (chosen by the agent; #93). A table, view or materialized view is in the definition and the fingerprint when the read login or the write login holds a privilege on it or on one of its columns; the pulling login (the structure login, or the read login) asks Postgres about those roles by name and needs no privilege on any table itself. What is not listed is counted (`hidden`), never named. The other candidate, listing what the pulling login can see, was not taken: it would make the definition depend on a login that by design holds nothing, and would need `REFERENCES` or the like on every table for a structure login. The consequence: code written against a table no login reaches fails the type-check at the next pull instead of failing `42501` when it runs.

**Logins are replaced in place, with nothing deployed again.** The new connection is sealed for the same row (ADR 0021), the pools are swapped, operations sent after the answer use the new logins, and the old connections are closed within 15 seconds. A Resource that was not served because its sealed value did not open is served at once when a whole connection is supplied.

**A Resource is removed without touching what it points at.** Its row and its Grants go; the shared database is not touched; a Storage Resource's files are never touched, only the program's own marker in the folder is removed so that the folder can be registered again. Applications that declare it stay deployed and their operations are refused `not-registered` (chosen by the agent; #93). A change of a Storage Resource's folder does not move files either.

**Every write of a Resource is one transaction that takes the Resource's row lock first, by the Resource's id**, and a write whose answer is lost is treated as a Grant's is ("A write whose answer is lost"): the Resource is not served on a guess, the row is read back whole, and a reading that finds no row lets the Resource go. What an answered change owes (its log record, the cleaning of the old folder) is done when the program finds out.

**Bounds** (chosen by the agent, technical): at most 1,000 Resources; `owner` and `contact` one line of at most 256 bytes, `description` at most 2,048 bytes; 64 KiB for the bodies of these requests; a check of logins 10 seconds in all, two checks at once in the program; the administrators' listing from memory, 100 to a page.

**Passwords reach the tool through its environment** (`VIBE_RESOURCE_READ_PASSWORD` and its two siblings), never the command line, as the OIDC client secret does (ADR 0018). The Dashboard has no form that takes a credential.

## What was considered and not taken

- **Registering a Resource whose login fails, with a warning.** Useful when a database is down for maintenance at that moment; it makes "registered" mean less, and the failure is then found by an Application's users. Put to the product owner in #93.
- **Refusing to remove a Resource that Applications declare.** It would make removal depend on Builders; the answer names the Applications instead.
- **Moving files when a folder changes.** A copy of unbounded size inside a request; the administrator moves them.
- **A warning or a refusal for Resources registered before this stage** whose read login can write: nothing looks again; the next registration or replacement of logins says it.

## How we know

- The contract's sections above, and the black-box tests that hold them (`resource_register_checked`, `resource_probe_warnings`, `resource_logins_replace`, `resource_edit`, `resource_remove`, `resource_remove_lost_answer`, `resource_lost_answer`, `storage_location`, `storage_location_refused`, `pull_limited_login`, `pull_limited_login_tool`, `admin_resources_list`, `admin_resources_refused`, `admin_resources_tool`, `admin_resources_tool_remove`): run green by the driving session on the tree that was landed (verified; the ticket's comment names the logs).
- That a role that is not a superuser may ask `has_table_privilege` and `has_any_column_privilege` about another role, and that a role the server does not have raises an error unless it is looked up in `pg_roles` first: reported by the contract's drafter from a throwaway Postgres 18 (not repeated by the driving session); the tests of `pull_limited_login` pull as a structure login that holds nothing and with a dropped role (verified by their run).
- The cost of the filter on a pull: reported by the implementer for a database of 2,000 tables (218 to 274 ms against 231 to 299 ms before; not repeated by the driving session).
