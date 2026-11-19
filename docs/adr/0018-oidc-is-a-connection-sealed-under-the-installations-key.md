---
status: accepted
---

# OIDC is a connection made from the tool, and the platform's secrets are sealed under a key given at install

The spec has a Platform administrator connect OIDC sign-in "through the command-line tool", authorized by a one-time setup credential because nobody can sign in before it is done (demonstration step 2; the ticket "The tool signs in through the browser; Platform administrators come from the directory"). The ticket "OIDC sign-in and per-subdomain sessions" had instead taken the issuer and the client id as options of `vibe serve` and the client secret from its environment, and had sealed refresh tokens under a key derived from that client secret. That was the building agent's default, made before this ticket was read closely; it does not fit the spec, and this ADR replaces it. Decided by the driving session on 2026-11-17 while writing the contract of the second ticket; the product owner may overrule it.

## The decision

**OIDC is a connection kept in the platform database.** A Platform administrator makes it with `vibe admin connect-oidc` (`PUT /v1/oidc`): the issuer, the client id, the client secret, the Groups claim, the scopes and the administrators' Group. It takes effect in the running program. `vibe serve` has no option for any of them. The platform asks the issuer for its configuration before it takes a connection, so a mistyped issuer is refused instead of locking everyone out.

**The first connection is made with a setup credential.** `vibe setup`, run beside the platform database by whoever installs, prints it once; the database keeps its digest. It is good for a day and for one connection, and ends when that is made. It is also the way back in when a connection turned out wrong: whoever holds the platform database's address is the installation's owner anyway.

**One key, given at install, seals every secret the platform keeps.** `VIBE_SECRET_KEY` (32 random bytes) is in the environment of `vibe serve` and never in the platform database. The client secret, refresh tokens and the `state` of sign-ins under way are sealed with AES-256-GCM under keys derived from it, one for each use. The ticket on Resources in full seals Resources' credentials under the same key. A dump of the platform database then holds no secret that can be used, which a client secret stored beside the tokens it protects could not give.

**The stand-in keeps the start-up options.** `--stand-in-sign-in` and `--administrators-group` stay options of `vibe serve` for tests and the local run, and cannot be used on a platform database that holds a connection.

## Considered Options

- **Keep the options of `vibe serve` and add nothing.** Simplest, and usual for self-hosted software. Rejected because the spec asks for the command, and because a connection changed by restarting with other options cannot be checked against the provider before it takes effect.
- **Both the options and the command.** Rejected: two ways to state one thing, and a rule needed for which wins.
- **The command stores everything but the client secret, which stays in the program's environment.** Keeps the secret out of the database without a new key. Rejected: connecting would take a command and a restart with a changed environment, and the ticket on Resources in full needs "a key that is not in the database" for Resources' credentials anyway.
- **The setup credential printed by `vibe serve` at its first start.** Rejected: a credential in the program's log, which every other rule keeps free of secrets, and no way to make another when it is lost.

## Consequences

- An installation needs `VIBE_SECRET_KEY` before OIDC can be connected. Losing it means connecting again (with `vibe setup`), and every session and tool credential ends at its next renewal.
- Changing the client secret at the provider is one command and ends no session.
- The harness of the black-box tests connects OIDC the way an administrator does.
