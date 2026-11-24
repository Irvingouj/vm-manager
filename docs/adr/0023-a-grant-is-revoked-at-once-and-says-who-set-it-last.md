---
status: proposed
---

# A Grant is revoked at once, says who set it last, and is changed from the tool or from its Resource's page

The spec (user stories 14 and 15) lets a Platform administrator take a Group's access to a Resource away and answer "who can reach what". The ticket "Resources and Grants in full" builds that as its third stage; ADR 0021 and ADR 0022 are the first two. `blackbox/CONTRACT.md`, "Resources and Grants in full" ("Grants in full", "The Dashboard: a Resource's page and its Grants"), is the interface this decision led to.

Status `proposed`: written on 2026-11-23. The choices marked "chosen by the agent" that are about what the product does for its users are put to the product owner in the issue "Decisions: how a Grant is given, revoked and remembered (ADR 0023)" (#95); the others are technical and stand until somebody finds a reason against them.

## What is decided

**A revoke holds at once, in every Application.** From the answer on, every operation that is decided is decided without the Grant: the next request, and the later operations of a request in flight. The ticket asked for "within one session renewal"; at once keeps that promise and is simpler to explain. It is possible because the program decides from the Grants it holds in memory, in step with the platform database (ADR 0015: one program serves an installation), and nothing caches a decision. What waits for a session's renewal is a User's Groups, which come from the company's directory, not the Grants of those Groups. A statement under way and a file being sent finish; a transaction already open is refused at its next statement.

**A revoke is `DELETE /v1/resources/<name>/grants/<group>`, and both writes of a Grant say what they changed** (`changed`, and `was`: the level held just before, read under the Resource's row lock). A revoke of a Grant that is not there answers `200` with `changed` false, so that asking again after an answer was lost is safe.

**A Grant records who set it last and when, and nothing earlier** (chosen by the agent). The Grant in force has `granted_by` and `granted_at`; earlier settings and every revoke are in the program's log (`grant-set`, `grant-revoked`, each with who made it) and nowhere else. A write whose outcome the program cannot tell writes `grant-unsettled` with who asked, so that a change that landed without an answer still has a trace. Not taken: a table of every change of every Grant, with a page and a command that list it. It is what an audit would want, and it is a feature of its own (what is kept, for how long, who reads it); the Operation log already records every operation made under a Grant.

**The order of a Resource's Grant writes is a counter on the Resource's row**, raised by every write of its Grants under the row's lock, a revoke included. Before this stage the program ordered what it read back against what it had written by the highest revision among the Grants themselves, which a revoke of the Grant written last would have lowered for good (the Resource would have stayed unknown until a restart).

**A Group is a name the administrator writes** (chosen by the agent). The program does not ask the company's directory whether a Group of that name exists, in the API, the tool or the Dashboard: the directory is reached only at sign-in, through the identity provider, and has no listing the program may read. A Group written wrongly holds a Grant nobody uses, and is seen in the listings.

**At most 1,000 Grants on a Resource** (chosen by the agent, technical), so that everything a listing answers and the program holds is bounded. A Resource that held more before keeps them.

**The tool has `vibe admin grant`, `revoke` and `grants`; the Dashboard has a page for each Resource with one form that gives or changes a Grant and a link to revoke each.** On the Dashboard a revoke is confirmed on a page of its own, as disabling an Application is (ADR 0009): it stops people at once, and a page with no script cannot ask "are you sure" any other way. The tool does not ask: the command names the Resource and the Group in full, and `grant` undoes it. The forms call what the API calls, so a change made in the browser is recorded exactly as the tool's is. Registering a Resource, replacing its logins, changing its location and removing it stay with the tool (ADR 0022: no credential is typed into a page).

## What was considered and not taken

- **Revoking with `POST .../grants` and `"access": null`.** One path fewer; a revoke then looks like a setting in every log and client, and a mistake in a client's JSON revokes.
- **A Grant that ends by itself, or needs a reason; a Grant to one User; the Resource's owner giving Grants.** None is in the spec for this milestone. The owner stays a text.
- **A form for each Resource on the list.** A hundred forms on a page; and no place for more than the first Grants of a Resource. The list links to the Resource's page instead.
- **Forms for the outbound list on the Dashboard.** Not in the ticket's criteria; the tool adds and removes an entry.

## How we know

- The contract's sections above and the black-box tests that hold them (`grant_set_changed`, `grant_revoke`, `grant_revoke_at_once`, `grant_revoke_refused`, `grant_lost_answer`, `admin_grants_by_group`, `admin_grants_tool`, and the `dashboard_admin_*` and `browser_*` files of the Dashboard's pages): the ticket's comment says which were run green by the driving session, and names the logs.
- That a revoke is at once without a renewal: `grant_revoke_at_once` starts the program with a session renewal of a day and is refused `no-grant` right after the answer (verified by its run).
- That a revoke of the Grant written last does not leave the Resource unknown: `grant_lost_answer` (verified by its run); the reasoning over the counter was read in the code by the driving session and by an independent reviewer, not proved.
