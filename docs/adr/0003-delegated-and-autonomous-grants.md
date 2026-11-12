# User and Application grants intersect; only Integrations act as themselves on shared Resources

When an Application reaches a Resource on behalf of a User (`ctx.user()`), the operation is allowed only if both the User and the Application hold a Grant on that Resource: the effective access is the intersection. An Application never holds an Autonomous grant on a shared Resource. The only thing it reaches as itself is its own database. Anything that must go beyond the User's own access is done by calling an Integration: separately trusted code that exposes a few narrow functions and holds the Autonomous grant itself. An Integration function is a Resource like any other, so calling it also needs a Grant on both the User and the calling Application.

We chose this so that neither a popular Application visited by a privileged User, nor a powerful Application opened by an unprivileged one, becomes a way around IT's intent, and so that privilege attaches to a function that does one thing instead of to an Application that can run any SQL. Who may open an Application then carries no risk and is left to its Builder; what IT reviews shrinks from Applications to Integration functions.

## Considered Options

- **Check the User only**: an Application inherits the access of whoever opens it. A team-poll Application with a poisoned dependency reads the finance database the moment the CFO opens it.
- **Check the Application only**: whoever can open an Application gets all of its access. A missing permission check in AI-written code lets an intern delete deployment records.
- **Autonomous grants on ordinary Applications, gated by who may open them**: the gate is a setting the Builder can widen later without IT noticing.

## Consequences

- IT maintains two sets of Grants (to Users or groups, and to Applications).
- "An employee with no access to the finance database can still submit a reimbursement" needs an Integration. Integrations are Milestone 2; in Milestone 1 acting as itself means an Application using its own database, nothing more.
- An Integration is only as trustworthy as the gate on publishing one. That gate is undecided.
- A refusal must say which side lacked the Grant, so that a Coding agent can tell a missing Grant from a bug.
