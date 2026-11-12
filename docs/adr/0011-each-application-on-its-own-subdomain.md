---
status: accepted
---

# Each Application is served on its own subdomain

Every deployed Application gets its own subdomain under one wildcard domain given to the installation, for example `expenses.apps.example.com`. The platform's own pages (sign-in, the Dashboard) have their own names under the same domain. Milestone 1 assumes the machine has been assigned a wildcard DNS name and a certificate for it; anything more flexible comes later.

Applications are written by many Builders and all of them share one sign-in. A browser treats different subdomains as different origins, which is what lets the platform keep one Application's front-end from acting in another Application as the same User. Under a single host with one path per Application, every Application's front-end would share an origin, and that separation would have to be rebuilt by hand.

## Considered Options

- **Paths under one host** (`apps.example.com/expenses`): simplest to install, since it needs one DNS name and one ordinary certificate. Rejected because all Applications would share a browser origin.
- **A custom domain per Application**: not needed for internal tools; can be added later on top of subdomains.

## Consequences

- Installation needs a wildcard DNS record and a wildcard certificate. This is one step more than "run the binary", and it is routine work for enterprise IT.
- Subdomains are necessary for the separation but not sufficient: sibling subdomains still count as the same site for cookies. The platform therefore must not share one session cookie across the wildcard domain, and must check which origin a request comes from. This is settled in Sign-in, sessions and Application URLs.
- An Application's name becomes part of its address, so names must be unique in the installation and valid as DNS labels.
- The local development loop needs an equivalent that works on a Builder's machine without DNS setup; this is for Local dev loop to settle.
