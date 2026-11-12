# User and Application grants intersect; acting as itself needs a separate grant

When an Application reaches a Resource on behalf of a User (`ctx.user()`), the operation is allowed only if both the User and the Application hold a Grant on that Resource: the effective access is the intersection. An Application can instead act as itself, ignoring the User's access, only where IT has given it a separate, explicit Autonomous grant. We chose this so that neither a popular Application visited by a privileged User, nor a powerful Application opened by an unprivileged one, becomes a way around IT's intent, and so that IT has one short list of the Applications allowed to act regardless of who is using them.

## Considered Options

- **Check the User only**: an Application inherits the access of whoever opens it. A team-poll Application with a poisoned dependency reads the finance database the moment the CFO opens it.
- **Check the Application only**: whoever can open an Application gets all of its access. A missing permission check in AI-written code lets an intern delete deployment records; the User's identity is left with an audit role only.

## Consequences

- IT maintains two sets of Grants (to Users or groups, and to Applications).
- "An employee with no access to the finance database can still submit a reimbursement" cannot go through `ctx.user()`. It needs an Autonomous grant, and then who may open that Application is the real gate, with business rules enforced by its own code.
- A refusal must say which side lacked the Grant, or Builders and Coding agents will reach for an Autonomous grant by reflex.
