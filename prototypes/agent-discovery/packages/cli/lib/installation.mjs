// The connection to the company's installation: who is signed in, the
// registered Resources and Grants, and the Applications.
import { existsSync, readFileSync, writeFileSync, realpathSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { VmError } from "./util.mjs";

export const LEVELS = ["none", "structure", "read", "write"];
export const rank = (level) => LEVELS.indexOf(level ?? "none");

export function vmHome() {
  return process.env.VM_HOME ? resolve(process.env.VM_HOME) : join(homedir(), ".vm");
}

function readConfig() {
  const file = join(vmHome(), "config.json");
  if (!existsSync(file)) {
    throw new VmError("You are not signed in to your company's platform. Run: npx vm login", { code: "not-signed-in" });
  }
  return JSON.parse(readFileSync(file, "utf8"));
}

export class Installation {
  constructor() {
    this.config = readConfig();
    const host = new URL(this.config.installation).host;
    this.dir = join(vmHome(), "installations", host);
    const catalogFile = join(this.dir, "catalog.json");
    if (!existsSync(catalogFile)) {
      throw new VmError(`Cannot reach ${this.config.installation}. Check your network and try again.`, { code: "unreachable" });
    }
    this.catalogPath = realpathSync(catalogFile);
    this.data = JSON.parse(readFileSync(this.catalogPath, "utf8"));
    this.stateFile = join(this.dir, "state.json");
    this.state = existsSync(this.stateFile) ? JSON.parse(readFileSync(this.stateFile, "utf8")) : { applications: {} };
  }

  get url() {
    return this.data.installation.url;
  }

  appAddress(name) {
    return `https://${name}.${this.data.installation.appsDomain}`;
  }

  /** Reads a file the installation serves, such as a Resource's structure. */
  file(rel) {
    return readFileSync(resolve(dirname(this.catalogPath), rel), "utf8");
  }

  me() {
    const id = this.config.account ?? this.data.signedIn;
    const u = this.data.users[id];
    if (!u) throw new VmError("Your session has expired. Run: npx vm login", { code: "session-expired" });
    return { id, ...u };
  }

  groupName(g) {
    return this.data.groups[g] ?? g;
  }

  /** The Builder's Access level on a Resource and the Groups that give it. */
  accessOf(resource, user = this.me()) {
    let best = "none";
    const via = [];
    for (const g of this.data.grants) {
      if (g.resource !== resource || !user.groups.includes(g.group)) continue;
      via.push({ group: g.group, access: g.access });
      if (rank(g.access) > rank(best)) best = g.access;
    }
    return { access: best, via };
  }

  resources() {
    return Object.entries(this.data.resources).map(([name, r]) => ({ name, ...r, ...this.accessOf(name) }));
  }

  resource(name) {
    const r = this.data.resources[name];
    return r ? { name, ...r, ...this.accessOf(name) } : null;
  }

  /** The person or Group label for "group:x" or a user id. */
  principalLabel(p) {
    if (p === "everyone") return "everyone in the company";
    if (p.startsWith("group:")) return `Group ${p.slice(6)} (${this.groupName(p.slice(6))})`;
    const u = this.data.users[p];
    return u ? `${u.displayName} <${u.email}>` : p;
  }

  matches(p, user) {
    if (p === "everyone") return true;
    if (p.startsWith("group:")) return user.groups.includes(p.slice(6));
    return p === user.id;
  }

  applications() {
    const me = this.me();
    const fromCatalog = this.data.applications.map((a) => ({ ...a }));
    for (const [name, a] of Object.entries(this.state.applications ?? {})) {
      const existing = fromCatalog.find((x) => x.name === name);
      const latest = a.versions[a.versions.length - 1];
      const merged = {
        name,
        description: latest.description,
        contributors: a.contributors,
        sharedWith: a.sharedWith ?? [],
        resources: latest.declared,
        version: latest.version,
        updated: latest.at.slice(0, 10),
      };
      if (existing) Object.assign(existing, merged);
      else fromCatalog.push(merged);
    }
    return fromCatalog
      .map((a) => {
        const contributor = a.contributors.some((p) => this.matches(p, me));
        const shared = a.sharedWith.some((p) => this.matches(p, me));
        return { ...a, relation: contributor ? "contributor" : shared ? "shared-with-you" : null, address: this.appAddress(a.name) };
      })
      .filter((a) => a.relation);
  }

  /** The installation-side record of an Application, including ones not visible to the Builder. */
  anyApplication(name) {
    return this.data.applications.find((a) => a.name === name) ?? (this.state.applications?.[name] ? { name, ...this.state.applications[name] } : null);
  }

  saveState() {
    mkdirSync(this.dir, { recursive: true });
    writeFileSync(this.stateFile, JSON.stringify(this.state, null, 2) + "\n");
  }
}
