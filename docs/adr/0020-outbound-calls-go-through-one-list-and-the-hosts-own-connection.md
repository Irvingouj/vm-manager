---
status: proposed
---

# Outbound calls are decided from one list of hosts, and the host makes the connection itself

The spec (user stories 24 and 25) gives Platform administrators "one platform-wide list of hosts Applications may call, empty by default, so that data does not leave through an Application's code", and wants "every outbound call logged with the Application, the User and the destination host". The ticket "Outbound calls: one platform-wide list of hosts, every call logged" adds that loopback, private and link-local addresses and the platform itself are refused whatever the list says, also when a listed name resolves to one, and that a redirect to a host not on the list is refused. `blackbox/CONTRACT.md`, "Outbound calls", is the interface this decision led to.

Status `proposed`: written by the driving session on 2026-11-23 before the implementation; the choices marked "chosen by the agent" are the product owner's to change.

## The decision

**An Application calls the outside with the standard `fetch`, and every such call is an operation of the Operation gateway.** Not a client of the SDK's own: code written for Workers, and every library that calls `fetch`, works unchanged. The vendored runtime already hands an outbound `fetch` to the host inside the Application's call (ADR 0002, patch 2), so the host decides it from the request in flight as it decides a query.

**The host makes the connection itself, with its own small client** (hyper's HTTP/1.1 client over rustls, both already in the program), and not with a general HTTP client library. The reasons are the ticket's own:

- *The address checked is the address connected to.* The host resolves the name, checks every address it was given against the ranges that are never called, and connects to one of those addresses. A library that resolves again when it connects can be given another answer the second time (DNS rebinding).
- *Every redirect is a decision.* The host follows redirects itself, one call at a time, each decided against the list and the addresses and each with its Operation log entry. A library that follows them inside itself would have to be told the rules, and a rule it did not ask about is a way out.
- *Bodies are streams in both directions*, on the machinery "Storage" built (ADR 0019): an answer's body is a stream the host feeds, a call's body is read as it is produced, and neither is held whole. `return fetch(url)` proxies without the bytes entering the Application's memory.
- *Nothing is shared between calls*: no connection kept, no cookie jar, no cache. One Application's call can leave nothing for another's to find.

**The list is of hosts, exact or one wildcard level (`*.example.com`), each with a port (443 unless written).** Only `https:`. Chosen by the agent:

- *Only `https:`.* The list says where data may go; over `http:` it would also go to everyone on the way. A company's internal `http:` service is inside (below), and is not what this ticket opens.
- *A port belongs to the entry.* A host listed for its API on 443 has not thereby been listed for whatever else listens on it.
- *Wildcards*, because the hosts of many services are per customer or per region (`<account>.example.com`), and an administrator asked for each one would list `*` if they could. `*.com` is not taken.

**The Operation log records the host and nothing else of the call.** Not the path and not the query: the acceptance criterion names the query string, and a path holds secrets as often (the address of an incoming webhook is one). Not the answer's status. Chosen by the agent: an administrator who needs more asks the Application's Contributors, whose code it is.

**Inside is never called, unless the machine's operator opens a range.** The ticket's ranges, the cloud's metadata address among them, are refused whatever the list says. Chosen by the agent: `vibe serve --outbound-private <range>` lets the person who starts the program name ranges of those kinds that may be called when the list allows the host. Two reasons. A company's "outside" is often partly inside (an API gateway on a private address), and Milestone 2's Integrations are far off. And the black-box tests need something to call: every address of a test machine is loopback or private. It is an option of the program's start and not of the list, so that a Platform administrator (a person in a Group at the identity provider) cannot point Applications at the installation's own network; who can start the program can already reach that network.

**The list is readable by everyone signed in; only Platform administrators change it.** A refusal names the host and the command an administrator runs, and `vibe outbound` shows what is allowed, so that a Coding agent stops and asks instead of trying another way out.

**Locally, the list is the project's own** (`outbound` in `vibe.dev.json`): the Builder decides what their own machine calls. An Application does not declare the hosts it calls in its Manifest: the spec has one list for the platform in Milestone 1 and rules per Application in Milestone 2.

## What was considered and not taken

- **A general HTTP client (`reqwest`)**: redirects, pools and resolution it does itself are what this ticket must decide itself; turning each of them off and replacing the resolver leaves the library's size without its help.
- **An egress proxy the installation runs beside the program** (the list enforced there): a second service to install and keep, for a product whose installation is one image; and the proxy would not know the User or the Application for the log. A company that already has an egress proxy is not served by this ticket either way ("What this does not close").
- **Resolving and checking, then letting the kernel connect by name**: the rebinding hole above.
- **Recording the path**: above.
- **Hosts declared in the Manifest and approved per Application**: Milestone 2, by the spec.

## Consequences

- An administrator who lists a host has judged that host, not the data sent to it: the platform does not look at bodies.
- HTTP/1.1 only, one connection a call: a chatty Application pays a TLS handshake a call. Measured when the first Application needs it; keeping connections is then a pool per host with the address checked again at every use.
- A company behind a mandatory egress proxy cannot use outbound calls until a later ticket.
- Without `signal` (the runtime has no `AbortController`), a call is given up only by its bounds and its request's end.
