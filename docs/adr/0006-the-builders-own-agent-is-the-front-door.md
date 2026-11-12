# The Builder's own agent is the front door; the platform has no builder interface

Any employee can be a Builder, and they build through the AI agent already on their machine (Claude Code, Codex, a desktop or coworking agent, Copilot). The platform does not ship a web page where apps are described and generated, and does not run a model. Its interface for building is a command-line tool and an MCP server designed to be used by an agent: simple enough that the agent installs them, signs in, discovers Resources, develops, and deploys on its own, with the Builder only describing what they want. The one step a person performs is signing in.

We chose this because the agent is already everywhere and already configured, so an in-platform builder would duplicate it while making the platform responsible for models, compute and cost.

## Consequences

- The tooling's first reader is an agent. Commands, output, refusals and instructions are written for it, and nothing a Builder does may require a person to click through a screen.
- The local development environment must be something an agent can install and start by itself on an ordinary office machine. What that demands (operating systems, no administrator rights, no container runtime) is unresolved and decides whether "any employee" is true.
- With every employee able to build, the number of Applications is expected to be large, which makes finding, owning and retiring Applications part of the product.
