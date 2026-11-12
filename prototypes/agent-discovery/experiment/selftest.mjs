#!/usr/bin/env node
// Self-test of the rig: sets up a trial directory and walks through it the way a
// careful Builder's agent would, printing PASS/FAIL per step.
//
//   node experiment/selftest.mjs [--keep]
//
// Uses its own VM_HOME and trial directory under the system temp folder, so it
// does not disturb a trial set up with the default ~/.vm.
import { spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
const keep = process.argv.includes("--keep");
const work = mkdtempSync(join(tmpdir(), "vm-selftest-"));
const trial = join(work, "project");
const env = { ...process.env, VM_HOME: join(work, "vm-home"), FORCE_COLOR: "0" };
const results = [];

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { cwd: trial, env, encoding: "utf8", timeout: 180000, ...opts });
  return { code: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}
const vmc = (...args) => run(join(trial, "node_modules", ".bin", "vm"), args);
const json = (r) => {
  try {
    return JSON.parse(r.out);
  } catch {
    return null;
  }
};

function step(name, fn) {
  const t = Date.now();
  let ok = false;
  let detail = "";
  try {
    const r = fn();
    ok = r === true || (r && r.ok === true);
    detail = r && r.detail ? r.detail : "";
  } catch (e) {
    detail = e.stack ?? String(e);
  }
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}  (${Date.now() - t} ms)`);
  if (!ok && detail) console.log(detail.split("\n").map((l) => "      " + l).join("\n"));
}
const expect = (cond, r) => ({ ok: !!cond, detail: r ? `exit ${r.code}\n${r.out.slice(0, 3000)}` : "" });

console.log(`trial directory: ${trial}\n`);

step("setup.sh creates the trial project", () => {
  const r = spawnSync(join(here, "setup.sh"), [trial], { env, encoding: "utf8", timeout: 600000 });
  return expect(r.status === 0 && existsSync(join(trial, "node_modules", ".bin", "vm")), { code: r.status, out: `${r.stdout}${r.stderr}` });
});

step("trial is a fresh Git repository with one commit", () => {
  const r = run("git", ["rev-list", "--count", "HEAD"]);
  return expect(r.code === 0 && r.out.trim() === "1", r);
});

step("nothing in the trial project reveals the rig", () => {
  const files = run("git", ["ls-files"]).out.trim().split("\n").filter((f) => !f.endsWith(".tgz"));
  const pkgs = ["node_modules/@vm/cli/lib", "node_modules/@vm/cli/bin", "node_modules/@vm/sdk"];
  const r = run("grep", ["-rniE", "fake|experiment|prototype|spike|vibe|vm-manager|selftest", ...files, ...pkgs]);
  return expect(r.code === 1, r);
});

step("vm whoami shows dana in Group facilities", () => {
  const r = vmc("whoami", "--json");
  return expect(json(r)?.user?.id === "dana" && json(r).groups.some((g) => g.id === "facilities"), r);
});

step("vm apps list shows incident-board, shared with the Builder", () => {
  const r = vmc("apps", "list", "--json");
  const a = json(r)?.find((x) => x.name === "incident-board");
  return expect(a && a.relation === "shared-with-you", r);
});

step("vm resources list shows every Resource with the Builder's access", () => {
  const r = vmc("resources", "list", "--json");
  const by = Object.fromEntries((json(r) ?? []).map((x) => [x.name, x.yourAccess]));
  return expect(by.facilities === "write" && by.devops === "read" && by.hr_directory === "structure" && by.payroll === "none", r);
});

step("vm resources pull devops writes resources/devops/schema.ts", () => {
  const r = vmc("resources", "pull", "devops");
  return expect(r.code === 0 && /pgTable\("incidents"/.test(readFileSync(join(trial, "resources/devops/schema.ts"), "utf8")), r);
});

step("vm resources pull payroll is refused (exit 2) and names whom to ask", () => {
  const r = vmc("resources", "pull", "payroll");
  return expect(r.code === 2 && /no Grant/.test(r.out) && /finance-systems@example\.com/.test(r.out) && !existsSync(join(trial, "resources/payroll")), r);
});

step("write a small Application (devops at read + own database)", () => {
  cpSync(join(here, "selftest", "app"), trial, { recursive: true });
  const dev = JSON.parse(readFileSync(join(trial, "vm.dev.json"), "utf8"));
  dev.users.push({ id: "outsider", displayName: "Outsider", email: "outsider@example.com", groups: [] });
  writeFileSync(join(trial, "vm.dev.json"), JSON.stringify(dev, null, 2) + "\n");
  return true;
});

step("vm check fails before the migration exists", () => {
  const r = vmc("check");
  return expect(r.code === 1 && /vm db generate/.test(r.out), r);
});

step("vm db generate writes the first migration", () => {
  const r = vmc("db", "generate");
  return expect(r.code === 0 && /CREATE TABLE "notes"/.test(r.out), r);
});

step("vm check passes (manifest, Resources, migrations, tsc incl. expected type errors)", () => {
  const r = vmc("check");
  return expect(r.code === 0 && /Check passed/.test(r.out), r);
});

step("vm dev request as dana: open incidents from devops (200)", () => {
  const r = vmc("dev", "request", "--as", "dana", "GET", "/incidents", "--json");
  const j = json(r);
  return expect(j?.status === 200 && Array.isArray(j.body) && j.body.some((i) => /Checkout latency/.test(i.title)), r);
});

step("vm dev request as outsider: refused, no Grant on devops (403)", () => {
  const r = vmc("dev", "request", "--as", "outsider", "GET", "/incidents", "--json");
  const j = json(r);
  return expect(j?.status === 403 && /holds no Grant on 'devops'/.test(j.body.fix), r);
});

step("own database: POST a note as dana, read it back", () => {
  const p = vmc("dev", "request", "--as", "dana", "POST", "/notes", "--data", '{"incidentId":1,"body":"vendor called"}', "--json");
  const g = vmc("dev", "request", "--as", "outsider", "GET", "/notes", "--json");
  const t = vmc("dev", "request", "--as", "dana", "GET", "/notes-in-tx", "--json");
  return expect(json(p)?.status === 201 && json(g)?.body?.[0]?.body === "vendor called" && json(t)?.body?.mine === 1, { code: 0, out: p.out + g.out + t.out });
});

step("run-time refusals around the types: not-declared, declared-lower", () => {
  const r = vmc("dev", "request", "--as", "dana", "GET", "/probe", "--json");
  const b = json(r)?.body;
  return expect(b?.undeclared === "not-declared" && b?.["write-through-read"] === "declared-lower", r);
});

step("vm db generate refuses a migration that drops a column (exit 2, nothing written)", () => {
  const f = join(trial, "src/db/schema.ts");
  const before = readFileSync(f, "utf8");
  writeFileSync(f, before.replace(/\n\s*body: text\("body"\)\.notNull\(\),/, ""));
  const r = vmc("db", "generate");
  writeFileSync(f, before);
  const files = run("ls", ["migrations"]).out;
  return expect(r.code === 2 && /drops column notes\.body/.test(r.out) && !/0001_/.test(files), r);
});

step("vm deploy refuses uncommitted changes", () => {
  const r = vmc("deploy");
  return expect(r.code === 1 && /Commit your changes first/.test(r.out), r);
});

step("vm deploy prints the Version and the address", () => {
  run("git", ["add", "-A"]);
  run("git", ["commit", "-qm", "Incident notes"]);
  const r = vmc("deploy");
  return expect(r.code === 0 && /Version 1/.test(r.out) && /https:\/\/incident-notes\.apps\.example\.com/.test(r.out), r);
});

step("the deploy bundle is a single classic script exposing fetch", () => {
  const code = readFileSync(join(trial, ".vm/build/app.js"), "utf8");
  const ctx = { Response, Request, Headers, URL, TextEncoder, TextDecoder, console };
  vm.createContext(ctx);
  vm.runInContext(code, ctx);
  return expect(!/^\s*(import|export)\s/m.test(code) && typeof ctx.__application?.default?.fetch === "function");
});

step("vm apps list now includes the deployed Application", () => {
  const r = vmc("apps", "list", "--json");
  return expect(json(r)?.some((a) => a.name === "incident-notes" && a.relation === "contributor"), r);
});

const failed = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - failed}/${results.length} steps passed.`);
if (!keep) rmSync(work, { recursive: true, force: true });
else console.log(`kept ${work} (VM_HOME=${env.VM_HOME})`);
process.exit(failed ? 1 : 0);
