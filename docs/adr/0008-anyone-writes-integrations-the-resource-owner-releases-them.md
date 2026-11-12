---
status: accepted, to be revisited with feedback from a real deployment
---

# Anyone can write an Integration; the Resource's owner or a top-level administrator releases it

An Integration is the one thing in the platform that acts beyond the access of the User it serves, so it is the one thing that needs a person's approval before it takes effect. Any employee can write one, through their agent, like any Application. Each function it exposes takes effect only when it has been released by the owner of the Resource it reaches, or by a top-level IT administrator. Approval is per function and per version: a changed function must be released again.

We chose this so that the supply of business capabilities is not limited by IT's time, while the decision to let others write to a Resource "in this particular way" stays with whoever answers for that Resource. Application deploys need no approval at all; the asymmetry is deliberate, because only an Integration steps around governance by identity.

## The weak point

The Resource's owner is often not a programmer. We assume they can understand what a narrow function does by having their own agent read it. That assumption is thin. It is the best available, and we will change the mechanism when a company using the platform tells us how it holds up.

## Considered Options

- **Only IT writes Integrations**: safest, and every business capability queues behind IT.
- **Anyone writes and it takes effect at once**: anyone can open a way around access control for themselves.
