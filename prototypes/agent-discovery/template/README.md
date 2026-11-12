# my-app

An internal Application on the company platform.

You normally do not need to read this: describe what you want to your coding agent, and it builds, tests and deploys the Application with the `vm` command. Its instructions are in `AGENTS.md`.

If you want to look yourself:

```
npm install                       # once
npx vm dev                        # run it on this machine, then open the address it prints
npx vm deploy                     # put the committed version live
```

- `manifest.ts`: the Application's name, description, and the company data (Resources) it uses.
- `src/`: the code. `src/db/schema.ts` is the Application's own database.
- `resources/`: the structure of company databases, pulled with `npx vm resources pull <name>`.
- `seed/` and `vm.dev.json`: test data and test Users for running it locally.

To use a company database you need access to it. `npx vm resources list` shows whom to ask.
