# Overview

Vibe Manager is an open-source, self-hosted platform on which an enterprise's employees and their AI coding agents build, deploy and run internal applications against shared, IT-governed infrastructure.

The latest release, `v{{version}}`, is still a minimal one, to try it. **It is not for untrusted Applications or real company data**: its [release notes](releases/) say what works and what is missing.

## Two people

There are two people in these documents. They can be the same person.

- The **Platform administrator** installs the platform on a server and connects it to the company's sign-in (OIDC).
- The **Builder** installs the tool `vibe` on their own computer, makes an Application, tries it there, deploys it and shares it.

[Getting started](getting-started/) takes both from nothing to an Application open in a browser, in one page.

## For Platform administrators

- [Installing with Compose](admin/install/): the files, the values, the certificate, the first start and the setup credential.
- [Connecting OIDC](admin/oidc/): registering the platform at the identity provider, `vibe admin connect-oidc`, signing in, and when a sign-in fails.
- [TLS](admin/tls/): the Compose file's reverse proxy, your own reverse proxy, or the program serving TLS itself.
- [Your own Postgres](admin/postgres/): a Postgres 18 of your own instead of the bundled one.
- [Upgrading and stopping](admin/updating/), and how the containers are hardened.

## For Builders

- [Making it and running it locally](builders/local-run/): `vibe new`, `vibe check`, `vibe dev`, `vibe request`.
- [Deploying](builders/deploying/): `vibe deploy`, and the Versions it makes.
- [Sharing](builders/sharing/): `vibe share`, and the Dashboard.
- [The tool's commands](builders/commands/): every command, as its `--help` prints it.
- [The SDK's types](builders/sdk/): what an Application is written with.

The words these pages use (Application, Resource, Grant, Version, Contributor, ...) are defined in the [Glossary](glossary/).
