import { existsSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { randomBytes } from "node:crypto";
import { Installation, rank, vmHome } from "./installation.mjs";
import { VmError, findProject, parseArgs, table, git, plural, projectRequire, readJson } from "./util.mjs";
import { evaluateManifest, requireManifest } from "./manifest.mjs";
import { checkCanPull, fetchStructure, prepareLocal, writePulled } from "./pull.mjs";
import { draft, journal, readMigration, dataLoss } from "./migrations.mjs";
import { bundle, pulledNames } from "./bundle.mjs";
import { LocalHost } from "./dev.mjs";

const VERSION = "0.4.1";

const HELP = {
  main: `vm ${VERSION}: build, run and deploy Applications on your company's platform.

Usage: npx vm <command> [options]

Finding what exists
  whoami                     Who you are signed in as, and your Groups
  apps list                  Applications you contribute to or that are shared with you
  apps show <name>           One Application: what it does, Contributors, Resources, address
  resources list             Every registered Resource, your Access level on it, and whom to ask
  resources pull <name>      Write resources/<name>/ with that Resource's structure

Building
  db generate                Write a migration to migrations/ from changes to src/db/schema.ts
  check                      Check the manifest, Resources, migrations and types
  dev                        Run the Application on this machine with test Users
  dev request                Send one request to the local Application as a test User

Shipping
  deploy                     Check, bundle and deploy a new Version

Every command takes --help and --json (machine-readable output).
Exit codes: 0 done, 1 failed, 2 refused or waiting for the Builder's decision.`,
  whoami: `Usage: npx vm whoami [--json]

Shows the employee you are signed in as and the directory Groups they belong to.
Grants are given to Groups, so your Groups decide which Resources you can reach.`,
  login: `Usage: npx vm login

Signs in to your company's platform in the browser. Signing in is the one step a person does.`,
  apps: `Usage:
  npx vm apps list [--json]
  npx vm apps show <name> [--json]

Lists the Applications you contribute to and the ones shared with you, so you can
check whether what you need already exists before building something new.`,
  resources: `Usage:
  npx vm resources list [--json]
  npx vm resources pull <name> [--json]

list   Every Resource registered by IT, with your Access level on it:
         none       you hold no Grant: you see only that it exists and whom to ask
         structure  you can pull its table structure and build against it, but not read rows
         read       you can read rows
         write      you can read and change rows
       Access comes from Grants to your Groups. Ask the contact shown to get one.

pull   Writes resources/<name>/schema.ts (the tables, as Drizzle definitions),
       relations.ts and index.ts, so ctx.user().db("<name>") is typed. These files are
       generated and read-only: pull again instead of editing them. Also writes
       seed/<name>.sql (test rows for the local stand-in) if it does not exist yet, and
       adds your Groups' Grants on it to vm.dev.json for local tests.
       Needs a Grant on the Resource (structure or more). Refused otherwise, exit code 2.`,
  check: `Usage: npx vm check [--json]

Checks, without deploying:
  - manifest.ts is a plain literal with a valid name, description and resources;
  - every declared Resource is registered, has been pulled, and you hold a Grant on it;
  - src/db/schema.ts has no changes missing from migrations/;
  - the project type-checks (tsc).`,
  db: `Usage: npx vm db generate [--name <slug>] [--allow-data-loss] [--json]

Compares src/db/schema.ts with migrations/ and writes the next migration for the
Application's own database. The migration is applied to the local database by
\`vm dev\` and to the real one by \`vm deploy\`.

A migration that destroys data (drops a table or column, changes a column's type)
is not written unless --allow-data-loss is given. That decision belongs to the
Builder: ask them, naming what will be lost, before using the flag.`,
  dev: `Usage:
  npx vm dev [--port <n>] [--reset]
  npx vm dev request [--as <test user>] <METHOD> <path> [--data <body>] [--header "Name: value"]... [--json]

dev          Runs the Application on this machine until stopped, with:
               - its own database, with every migration in migrations/ applied;
               - a local stand-in for each pulled Resource, built from resources/<name>/
                 and filled from seed/<name>.sql (no real company data);
               - the test Users, Groups and Grants in vm.dev.json.
             The same rules as the platform apply: a Resource not declared in manifest.ts,
             declared at too low a level, or a test User without a Grant is refused.
             Open the printed address and pick a test User to sign in as.
             --reset  clears the local databases and rebuilds them.

dev request  Sends one request as a test User and prints the response. Uses the running
             \`vm dev\` if there is one, otherwise starts the Application just for this request.
             --as defaults to the first test User in vm.dev.json.
             Example: npx vm dev request --as dana POST /notes --data '{"body":"hi"}'`,
  deploy: `Usage: npx vm deploy [--allow-data-loss] [--json]

Deploys the committed state of this project as a new Version:
  1. pulls the current structure of every declared Resource again;
  2. runs the same checks as \`vm check\`;
  3. bundles the Application;
  4. sends the Version (bundle, Declared access, pending migrations, Git commit).
The platform applies pending migrations, then switches the address to the new Version.
Commit your changes first. A pending migration that destroys data needs
--allow-data-loss, which only the Builder may decide.`,
};

let JSON_MODE = false;

function print(json, text) {
  if (JSON_MODE) console.log(JSON.stringify(json, null, 2));
  else if (text !== undefined && text !== null) console.log(text);
}

export async function main(argv) {
  const { positionals, flags } = parseArgs(argv, ["as", "port", "data", "header", "name"]);
  JSON_MODE = !!flags.json;
  const [cmd, sub, ...rest] = positionals;
  if (flags.version || cmd === "version") {
    print({ version: VERSION }, `vm ${VERSION}`);
    return 0;
  }
  if (!cmd || cmd === "help") {
    console.log(HELP[sub] ?? HELP.main);
    return 0;
  }
  if (flags.help) {
    if (!HELP[cmd]) {
      console.log(HELP.main);
      return 1;
    }
    console.log(HELP[cmd]);
    return 0;
  }
  try {
    switch (cmd) {
      case "whoami":
        return await whoami();
      case "login":
        return await login();
      case "logout":
        print({ ok: false }, "Signing out is done from the Dashboard in this version.");
        return 1;
      case "apps":
        if (sub === "list" || !sub) return await appsList();
        if (sub === "show") return await appsShow(rest[0]);
        break;
      case "resources":
        if (sub === "list" || !sub) return await resourcesList();
        if (sub === "pull") return await resourcesPull(rest[0]);
        break;
      case "check":
        return await checkCommand();
      case "db":
        if (sub === "generate") return await dbGenerate(flags);
        break;
      case "dev":
        if (sub === "request") return await devRequest(rest, flags);
        if (!sub) return await devServe(flags);
        break;
      case "deploy":
        return await deploy(flags);
    }
    console.error(`Unknown command: ${positionals.join(" ")}\n`);
    console.error(HELP[cmd] ?? HELP.main);
    return 1;
  } catch (e) {
    if (e instanceof VmError) {
      if (JSON_MODE) console.log(JSON.stringify({ ok: false, error: e.code, message: e.message, ...(e.details ?? {}) }, null, 2));
      else console.error(e.message);
      return e.exit;
    }
    throw e;
  }
}

// ------------------------------------------------------------ discovery

function whoami() {
  const inst = new Installation();
  const me = inst.me();
  print(
    { installation: inst.url, user: { id: me.id, displayName: me.displayName, email: me.email }, groups: me.groups.map((g) => ({ id: g, name: inst.groupName(g) })) },
    [`Signed in to ${inst.url} as ${me.displayName} <${me.email}> (${me.id})`, `Groups: ${me.groups.map((g) => `${g} (${inst.groupName(g)})`).join(", ")}`].join("\n"),
  );
  return 0;
}

function login() {
  const inst = new Installation();
  const me = inst.me();
  print({ installation: inst.url, user: me.id, alreadySignedIn: true }, `Already signed in to ${inst.url} as ${me.displayName} (${me.id}).`);
  return 0;
}

function appResources(inst, a) {
  return Object.entries(a.resources).map(([name, access]) => {
    const mine = inst.accessOf(name).access;
    return { name, access, yourAccess: mine, refusedForYou: rank(mine) < rank(access) };
  });
}

function appsList() {
  const inst = new Installation();
  const apps = inst.applications();
  const json = apps.map((a) => ({
    name: a.name,
    description: a.description,
    relation: a.relation,
    address: a.address,
    contributors: a.contributors.map((p) => inst.principalLabel(p)),
    resources: appResources(inst, a),
  }));
  const blocks = json.map((a) =>
    [
      `${a.name}  (${a.relation === "contributor" ? "you contribute" : "shared with you"})`,
      `  ${a.description}`,
      `  Address:      ${a.address}`,
      `  Contributors: ${a.contributors.join(", ")}`,
      `  Resources:    ${a.resources.map((r) => `${r.name} (${r.access})${r.refusedForYou ? " [you hold " + r.yourAccess + "]" : ""}`).join(", ") || "none"}`,
    ].join("\n"),
  );
  print(json, json.length ? blocks.join("\n\n") + `\n\n${plural(json.length, "Application")}. Details: npx vm apps show <name>` : "No Applications are shared with you yet.");
  return 0;
}

function appsShow(name) {
  if (!name) throw new VmError("Usage: npx vm apps show <name>");
  const inst = new Installation();
  const a = inst.applications().find((x) => x.name === name);
  if (!a) throw new VmError(`No Application named '${name}' is shared with you or lists you as a Contributor. Run: npx vm apps list`, { code: "not-found" });
  const resources = appResources(inst, a);
  const json = {
    name: a.name,
    description: a.description,
    relation: a.relation,
    address: a.address,
    version: a.version,
    updated: a.updated,
    contributors: a.contributors.map((p) => inst.principalLabel(p)),
    sharedWith: a.sharedWith.map((p) => inst.principalLabel(p)),
    resources,
  };
  const lines = [
    `${a.name}: ${a.description}`,
    `Address:      ${a.address}`,
    `You:          ${a.relation === "contributor" ? "Contributor (you may change and deploy it)" : "shared with you (you may open it)"}`,
    `Version:      ${a.version}, deployed ${a.updated}`,
    `Contributors: ${json.contributors.join(", ")}`,
    `Shared with:  ${json.sharedWith.join(", ") || "Contributors only"}`,
    `Resources:`,
    ...resources.map(
      (r) => `  ${r.name.padEnd(14)} declared ${r.access.padEnd(6)} you hold ${r.yourAccess}${r.refusedForYou ? `  <- the parts that ${r.access === "write" && r.yourAccess === "read" ? "change" : "read"} ${r.name} are refused for you` : ""}`,
    ),
  ];
  if (!resources.length) lines.push("  none");
  print(json, lines.join("\n"));
  return 0;
}

function resourcesList() {
  const inst = new Installation();
  const rs = inst.resources();
  const json = rs.map((r) => ({
    name: r.name,
    kind: r.kind,
    description: r.description,
    yourAccess: r.access,
    via: r.via.map((v) => `${v.group}:${v.access}`),
    owner: r.owner,
    ask: r.ask,
  }));
  const blocks = rs.map((r) =>
    [
      `${r.name}  (${r.kind})  your access: ${r.access}${r.via.length ? ` (through Group ${r.via.map((v) => v.group).join(", ")})` : ""}`,
      `  ${r.description}`,
      `  Owner: ${r.owner}. Whom to ask: ${r.ask}`,
    ].join("\n"),
  );
  print(
    json,
    blocks.join("\n\n") +
      "\n\nAccess levels: none = only its existence is visible; structure = pull its tables, no rows; read; write.\nPull a Resource's structure with: npx vm resources pull <name>",
  );
  return 0;
}

async function resourcesPull(name) {
  if (!name) throw new VmError("Usage: npx vm resources pull <name>");
  const root = findProject();
  const inst = new Installation();
  const r = checkCanPull(inst, name);
  const pulled = await fetchStructure(inst, name, root);
  const w = writePulled(root, inst, name, pulled);
  const notes = prepareLocal(root, inst, name);
  const declared = evaluateManifest(root).manifest?.resources?.[name];
  const next = declared
    ? []
    : [`Not declared in manifest.ts yet. To use it, add:  ${name}: { kind: "postgres", access: "read" }  (or "write")`];
  const accessNote =
    r.access === "structure"
      ? `Your access is 'structure': you can build against these tables, but reading rows is refused for you and for anyone without a 'read' Grant on '${name}', locally and once deployed. Whom to ask: ${r.ask}`
      : `Your access: ${r.access}.`;
  print(
    { ok: true, resource: name, yourAccess: r.access, tables: pulled.tables, fingerprint: pulled.fingerprint, files: [`resources/${name}/schema.ts`, `resources/${name}/relations.ts`, `resources/${name}/index.ts`], changed: w.changed, notes },
    [
      `Pulled the structure of '${name}' into resources/${name}/ (${plural(pulled.tables.length, "table")}: ${pulled.tables.join(", ")}).`,
      w.changed ? "The structure changed since the last pull." : null,
      accessNote,
      ...notes,
      ...next,
    ]
      .filter(Boolean)
      .join("\n"),
  );
  return 0;
}

// ------------------------------------------------------------ check

function runTsc(root) {
  let tscBin;
  try {
    tscBin = join(projectRequire(root).resolve("typescript/package.json"), "..", "bin", "tsc");
  } catch {
    return { ok: false, output: "TypeScript is not installed in this project. Run: npm install" };
  }
  const r = spawnSync(process.execPath, [tscBin, "--noEmit", "-p", "tsconfig.json", "--pretty", "false"], { cwd: root, encoding: "utf8" });
  return { ok: r.status === 0, output: `${r.stdout}${r.stderr}`.trim() };
}

async function runChecks(root, inst, { skipTsc = false } = {}) {
  const errors = [];
  const warnings = [];
  const { manifest, errors: mErr } = evaluateManifest(root);
  errors.push(...mErr.map((m) => `manifest.ts: ${m}`));
  if (manifest?.resources) {
    for (const [name, d] of Object.entries(manifest.resources)) {
      const r = inst.resource(name);
      if (!r) {
        errors.push(`'${name}' is declared in manifest.ts but no Resource has that name. Run: npx vm resources list`);
        continue;
      }
      if (r.access === "none") {
        errors.push(`You hold no Grant on '${name}', so the Application cannot be built against it. Whom to ask: ${r.ask}`);
        continue;
      }
      if (!existsSync(join(root, "resources", name, "schema.ts"))) {
        errors.push(`The structure of '${name}' has not been pulled. Run: npx vm resources pull ${name}`);
      }
      if (rank(r.access) < rank(d.access)) {
        warnings.push(
          `'${name}' is declared at '${d.access}' and you hold '${r.access}'. The Application can be deployed, but its ${d.access === "write" ? "writes to" : "reads of"} '${name}' are refused for you and for every User whose Groups do not hold '${d.access}'. Make sure the people who will use it hold that Grant. Whom to ask: ${r.ask}`,
        );
      }
    }
  }
  try {
    const d = draft(root);
    d.discard();
    if (d.added.length) errors.push("src/db/schema.ts has changes that are not in migrations/. Run: npx vm db generate");
  } catch (e) {
    if (!(e instanceof VmError)) throw e;
    errors.push(e.message);
  }
  let tsc = { ok: true, output: "" };
  if (!skipTsc) {
    tsc = runTsc(root);
    if (!tsc.ok) errors.push(`Type errors (tsc):\n${tsc.output.split("\n").map((l) => "  " + l).join("\n")}`);
  }
  return { ok: errors.length === 0, errors, warnings, manifest };
}

async function checkCommand() {
  const root = findProject();
  const inst = new Installation();
  const res = await runChecks(root, inst);
  const text = [
    ...res.errors.map((e) => `error: ${e}`),
    ...res.warnings.map((w) => `warning: ${w}`),
    res.ok ? `Check passed${res.warnings.length ? ` with ${plural(res.warnings.length, "warning")}` : ""}: manifest, ${plural(Object.keys(res.manifest?.resources ?? {}).length, "Resource")}, migrations, types.` : `Check failed: ${plural(res.errors.length, "problem")}.`,
  ];
  print({ ok: res.ok, errors: res.errors, warnings: res.warnings }, text.join("\n"));
  return res.ok ? 0 : 1;
}

// ------------------------------------------------------------ own database

async function dbGenerate(flags) {
  const root = findProject();
  const d = draft(root, { name: typeof flags.name === "string" ? flags.name : undefined });
  if (d.added.length === 0) {
    d.discard();
    print({ ok: true, generated: [] }, "No changes in src/db/schema.ts since the last migration. Nothing generated.");
    return 0;
  }
  const loses = d.added.flatMap((a) => a.loses.map((l) => ({ file: a.file, what: l })));
  if (loses.length && !flags["allow-data-loss"]) {
    d.discard();
    print(
      { ok: false, error: "data-loss", needsConfirmation: true, loses },
      [
        "Stopped: this migration destroys data.",
        ...loses.map((l) => `  - it ${l.what}`),
        "The data cannot be recovered once this is deployed: going back to an earlier Version does not undo a migration.",
        "Ask the Builder whether that data may be lost, and tell them exactly what is listed above.",
        "Only if they confirm, run: npx vm db generate --allow-data-loss",
        "Nothing was written.",
      ].join("\n"),
    );
    return 2;
  }
  d.commit();
  print(
    { ok: true, generated: d.added.map((a) => ({ file: a.file, sql: a.sql, loses: a.loses })) },
    d.added
      .map((a) => [`Wrote ${a.file}:`, ...a.sql.split("--> statement-breakpoint").map((s) => "  " + s.trim().replace(/\n/g, "\n  ")), ...a.loses.map((l) => `  (destroys data: it ${l})`)].join("\n"))
      .join("\n") + "\nIt is applied to the local own database by `vm dev`, and to the real one by `vm deploy`.",
  );
  return 0;
}

// ------------------------------------------------------------ dev

function serverFile(root) {
  return join(root, ".vm", "dev", "server.json");
}

function runningServer(root) {
  const f = serverFile(root);
  if (!existsSync(f)) return null;
  try {
    const s = readJson(f);
    process.kill(s.pid, 0);
    return s;
  } catch {
    return null;
  }
}

function describeStart(summary, name, address) {
  const lines = [];
  const m = summary.migrations;
  lines.push(`Own database: ${plural(m.total, "migration")} in migrations/${m.applied.length ? `, ${m.applied.length} applied now` : ""}.`);
  for (const s of summary.standins) {
    const decl = summary.manifest.resources[s.name];
    lines.push(`Local stand-in '${s.name}': ${plural(s.rows, "row")} from seed/${s.name}.sql${decl ? `, declared at ${decl.access}` : ", not declared in manifest.ts"}.`);
  }
  for (const n of summary.missing) lines.push(`warning: '${n}' is declared but not pulled; its operations are refused. Run: npx vm resources pull ${n}`);
  lines.push(`Test Users (vm.dev.json): ${summary.users.map((u) => `${u.id} [${u.groups.join(", ") || "no Groups"}]`).join(", ")}`);
  if (address) lines.push(`\n${name} is running at ${address}`);
  return lines.join("\n");
}

async function devServe(flags) {
  const root = findProject();
  if (runningServer(root)) throw new VmError("`vm dev` is already running for this project. Use it, or stop it first.");
  const host = new LocalHost(root, { log: (l) => console.log(l) });
  await host.open({ reset: !!flags.reset });
  let summary;
  try {
    summary = await host.refresh();
  } catch (e) {
    await host.close();
    if (e.buildErrors || e instanceof VmError) throw new VmError(e.message);
    throw e;
  }
  const name = summary.manifest.name;
  const token = randomBytes(16).toString("hex");
  const port = await listen(host, name, token, Number(flags.port ?? 8787));
  const address = `http://${name}.localhost:${port}`;
  writeFileSync(serverFile(root), JSON.stringify({ pid: process.pid, port, token, address }));
  console.log(describeStart(summary, name, address));
  console.log(`Open it in a browser and pick a test User, or from a terminal:\n  npx vm dev request --as ${summary.users[0].id} GET /\nChanges to the project are picked up on the next request. Press Ctrl+C to stop.`);
  const stop = async () => {
    rmSync(serverFile(root), { force: true });
    await host.close();
    process.exit(0);
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  await new Promise(() => {});
}

function listen(host, name, token, port) {
  const server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, `http://${name}.localhost`);
      const cookies = Object.fromEntries((req.headers.cookie ?? "").split(/;\s*/).filter(Boolean).map((c) => c.split(/=(.*)/s)));
      if (url.pathname === "/__vm/sign-in") {
        const summary = await host.refresh();
        const next = url.searchParams.get("next") ?? "/";
        const pick = url.searchParams.get("as");
        if (pick) {
          res.writeHead(302, { location: next, "set-cookie": `vm_dev_user=${encodeURIComponent(pick)}; Path=/; HttpOnly; SameSite=Lax` });
          return res.end();
        }
        res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
        return res.end(
          `<!doctype html><title>Sign in (local)</title><h1>Sign in to ${name} (local)</h1><p>Pick a test User from vm.dev.json:</p><ul>${summary.users
            .map((u) => `<li><a href="/__vm/sign-in?as=${encodeURIComponent(u.id)}&next=${encodeURIComponent(next)}">${u.displayName} (${u.id}) — Groups: ${u.groups.join(", ") || "none"}</a></li>`)
            .join("")}</ul>`,
        );
      }
      if (url.pathname === "/__vm/sign-out") {
        res.writeHead(302, { location: "/", "set-cookie": "vm_dev_user=; Path=/; Max-Age=0" });
        return res.end();
      }
      let userId = null;
      if (req.headers["x-vm-dev-token"] === token && req.headers["x-vm-dev-user"]) userId = String(req.headers["x-vm-dev-user"]);
      else if (cookies.vm_dev_user) userId = decodeURIComponent(cookies.vm_dev_user);
      if (!userId) {
        res.writeHead(302, { location: `/__vm/sign-in?next=${encodeURIComponent(url.pathname + url.search)}` });
        return res.end();
      }
      const chunks = [];
      for await (const c of req) chunks.push(c);
      const headers = new Headers();
      for (const [k, v] of Object.entries(req.headers)) if (!k.startsWith("x-vm-dev-") && v !== undefined) headers.set(k, Array.isArray(v) ? v.join(", ") : v);
      const request = new Request(url, { method: req.method, headers, body: ["GET", "HEAD"].includes(req.method) ? undefined : Buffer.concat(chunks) });
      console.log(`${req.method} ${url.pathname}${url.search} as ${userId}`);
      const response = await host.handle(request, userId);
      const body = Buffer.from(await response.arrayBuffer());
      console.log(`  -> ${response.status}`);
      res.writeHead(response.status, Object.fromEntries(response.headers));
      res.end(body);
    } catch (e) {
      console.log(`  -> 500 ${e.message}`);
      res.writeHead(500, { "content-type": "text/plain; charset=utf-8" });
      res.end(e instanceof VmError ? e.message : String(e.stack ?? e));
    }
  });
  return new Promise((resolve, reject) => {
    let p = port;
    server.on("error", (e) => {
      if (e.code === "EADDRINUSE" && p < port + 20) server.listen(++p, "127.0.0.1");
      else reject(e);
    });
    server.on("listening", () => resolve(p));
    server.listen(p, "127.0.0.1");
  });
}

async function devRequest(args, flags) {
  const root = findProject();
  let [method, path] = args;
  if (method && !path && method.startsWith("/")) [method, path] = ["GET", method];
  if (!method || !path) throw new VmError("Usage: npx vm dev request [--as <test user>] <METHOD> <path> [--data <body>] [--header \"Name: value\"]");
  method = method.toUpperCase();
  if (!path.startsWith("/")) path = "/" + path;
  const headers = new Headers();
  for (const h of [].concat(flags.header ?? [])) {
    const i = h.indexOf(":");
    if (i < 1) throw new VmError(`--header must look like "Name: value" (got ${h}).`);
    headers.set(h.slice(0, i).trim(), h.slice(i + 1).trim());
  }
  const body = typeof flags.data === "string" ? flags.data : undefined;
  if (body !== undefined && !headers.has("content-type")) headers.set("content-type", /^\s*[[{]/.test(body) ? "application/json" : "text/plain");
  const dev = readJson(join(root, "vm.dev.json"));
  const as = typeof flags.as === "string" ? flags.as : dev.users?.[0]?.id;

  let response;
  const server = runningServer(root);
  if (server) {
    headers.set("x-vm-dev-user", as);
    headers.set("x-vm-dev-token", server.token);
    response = await fetch(`http://127.0.0.1:${server.port}${path}`, { method, headers, body, redirect: "manual" });
  } else {
    const host = new LocalHost(root);
    await host.open();
    try {
      const name = requireManifest(root).name;
      response = await host.handle(new Request(`http://${name}.localhost${path}`, { method, headers, body }), as);
    } catch (e) {
      if (e.buildErrors || e instanceof VmError) throw new VmError(e.message);
      throw e;
    } finally {
      await host.close();
    }
  }
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {}
  const out = [`${method} ${path} as ${as} -> ${response.status} ${response.statusText || ""}`.trimEnd(), `content-type: ${response.headers.get("content-type") ?? "(none)"}`];
  if (response.headers.get("location")) out.push(`location: ${response.headers.get("location")}`);
  out.push("", parsed !== null ? JSON.stringify(parsed, null, 2) : text);
  if (response.status === 403 && parsed?.refusal) out.push("", `Refused (${parsed.refusal.reason}): ${parsed.fix}`);
  print({ request: { method, path, as }, status: response.status, headers: Object.fromEntries(response.headers), body: parsed ?? text }, out.join("\n"));
  return 0;
}

// ------------------------------------------------------------ deploy

async function deploy(flags) {
  const root = findProject();
  const inst = new Installation();
  const me = inst.me();
  const manifest = requireManifest(root);
  const name = manifest.name;

  const inRepo = git(root, ["rev-parse", "--is-inside-work-tree"]);
  if (!inRepo.ok) throw new VmError("This project is not a Git repository. A Version records the Git commit it was built from: run `git init` and commit first.");
  const dirty = git(root, ["status", "--porcelain"]).out.split("\n").filter((l) => l && !l.slice(3).startsWith(".vm/"));
  if (dirty.length) {
    throw new VmError(
      [`Commit your changes first: a Version records the Git commit it was built from. Not committed:`, ...dirty.slice(0, 15).map((l) => "  " + l)].join("\n"),
      { code: "uncommitted" },
    );
  }
  const head = git(root, ["rev-parse", "HEAD"]);
  if (!head.ok) throw new VmError("There is no commit yet. Commit the project first.");
  const commit = head.out;

  const existing = inst.anyApplication(name);
  if (existing) {
    const contributors = existing.contributors ?? [];
    if (!contributors.some((p) => inst.matches(p, me))) {
      throw new VmError(
        `The name '${name}' belongs to an existing Application you do not contribute to (Contributors: ${contributors.map((p) => inst.principalLabel(p)).join(", ")}). Choose another name in manifest.ts, or ask one of its Contributors to add you.`,
        { code: "name-taken", exit: 2 },
      );
    }
  }

  const steps = [];
  for (const rname of Object.keys(manifest.resources)) {
    const r = inst.resource(rname);
    if (!r) continue;
    checkCanPull(inst, rname);
    const pulled = await fetchStructure(inst, rname, root);
    const w = writePulled(root, inst, rname, pulled);
    if (w.changed || w.created) steps.push(`The structure of '${rname}' changed since your last pull; resources/${rname}/ was updated. Commit it.`);
  }

  const res = await runChecks(root, inst);
  if (!res.ok) {
    throw new VmError([...res.errors.map((e) => `error: ${e}`), "Deploy stopped: nothing was sent."].join("\n"), { code: "check-failed", details: { errors: res.errors } });
  }

  const state = inst.state.applications?.[name];
  const appliedBefore = new Set(state?.migrations ?? []);
  const pending = journal(join(root, "migrations")).filter((e) => !appliedBefore.has(e.tag));
  const loses = state ? pending.flatMap((e) => dataLoss(readMigration(join(root, "migrations"), e.tag)).map((what) => ({ file: `migrations/${e.tag}.sql`, what }))) : [];
  if (loses.length && !flags["allow-data-loss"]) {
    print(
      { ok: false, error: "data-loss", needsConfirmation: true, loses },
      [
        `Stopped: the pending migrations destroy data in ${name}'s database, which holds real data since its last Version.`,
        ...loses.map((l) => `  - ${l.file} ${l.what}`),
        "Going back to an earlier Version does not undo this. Ask the Builder whether this data may be lost, naming exactly what is listed above.",
        "Only if they confirm, run: npx vm deploy --allow-data-loss",
        "Nothing was sent.",
      ].join("\n"),
    );
    return 2;
  }

  let out;
  try {
    out = await bundle(root, { mode: "deploy" });
  } catch (e) {
    throw new VmError(`${e.message}\nDeploy stopped: nothing was sent.`);
  }

  inst.state.applications ??= {};
  const record = (inst.state.applications[name] ??= {
    contributors: [me.id],
    sharedWith: [],
    versions: [],
    migrations: [],
  });
  if (existing && !inst.state.applications[name].versions.length && existing.version) record.base = existing.version;
  const version = (record.versions.at(-1)?.version ?? record.base ?? 0) + 1;
  record.versions.push({
    version,
    commit,
    at: new Date().toISOString(),
    description: manifest.description,
    declared: Object.fromEntries(Object.entries(manifest.resources).map(([k, v]) => [k, v.access])),
    migrations: pending.map((e) => e.tag),
    bytes: out.bytes,
  });
  record.migrations = [...appliedBefore, ...pending.map((e) => e.tag)];
  inst.saveState();

  const address = inst.appAddress(name);
  const declared = Object.entries(manifest.resources).map(([k, v]) => `${k} (${v.access})`).join(", ") || "none";
  print(
    { ok: true, application: name, version, commit, address, declaredAccess: manifest.resources, migrationsApplied: pending.map((e) => e.tag), bundleBytes: out.bytes, warnings: res.warnings, notes: steps },
    [
      ...steps,
      ...res.warnings.map((w) => `warning: ${w}`),
      `Checked: manifest, ${plural(Object.keys(manifest.resources).length, "Resource")}, migrations, types.`,
      `Bundled: ${(out.bytes / 1024).toFixed(1)} KB (${(out.gzip / 1024).toFixed(1)} KB gzipped).`,
      `Pushed commit ${commit.slice(0, 7)}.`,
      `Declared access: ${declared}.`,
      pending.length ? `Migrations applied: ${pending.map((e) => e.tag).join(", ")}.` : "Migrations applied: none.",
      `Deployed ${name} Version ${version}.`,
      `Address: ${address}`,
      version === 1 ? "It is visible to its Contributors only. Share it from the Dashboard." : null,
    ]
      .filter(Boolean)
      .join("\n"),
  );
  return 0;
}

export { vmHome };
