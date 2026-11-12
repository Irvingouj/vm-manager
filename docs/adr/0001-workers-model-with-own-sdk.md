# Workers execution model with our own identity-aware SDK

An Application is a stateless program in the Cloudflare Workers execution model: a web-standard request handler that may serve anything web standards allow. It reaches enterprise infrastructure only through our own SDK (`ctx.user().db("devops")`), and Cloudflare's binding APIs (`env.DB`, D1, R2, KV, wrangler config) are deliberately not emulated. We chose this because every Resource operation must carry identity, and Cloudflare's binding APIs have no place to put the User.

## Considered Options

- **Cloudflare API compatibility**: existing Worker code would run unmodified, but identity would have to be retrofitted onto identity-less APIs, D1 speaks SQLite while our databases are Postgres, and we would chase Cloudflare's surface indefinitely.
- **A platform-specific split into handler plus static assets**: rejected as an artificial limit. The Application decides what it serves.
