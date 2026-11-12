# Applications belong to the company: Contributors instead of owners, source kept in Git on the platform

An Application has no owner. It belongs to the company, and the only thing controlled is who may contribute to it, meaning change and deploy it: at first the Builder who created it, then whichever people or Groups are added. Who may open an Application is a separate list.

For contribution to be real, the source has to be somewhere every Contributor's agent can reach. By default it lives in a Git repository hosted by the platform itself: a lightweight Git server ships inside the platform, so nothing else has to be installed and no employee needs an account elsewhere. An Application can instead be configured to deploy from an external Git source, GitHub or any other. We chose Git because agents already use it fluently, so the employee never has to, and because it gives version history for free.

## Considered Options

- **A person or a Group owns each Application**: forces an assumption about who will care for an Application later, which cannot be known when it is created.
- **Source stays on the Builder's machine, only the built bundle is uploaded**: nobody else can contribute, and the source is lost when the Builder moves on.
- **Require an external Git host**: most employees have no account on one.

## Consequences

- The platform keeps Applications as well as running them.
- Who pushed a change is recorded by the platform from the signed-in identity, not taken from what the commit claims.
- Deploying from a Git source raises where the build happens, which is unresolved.
