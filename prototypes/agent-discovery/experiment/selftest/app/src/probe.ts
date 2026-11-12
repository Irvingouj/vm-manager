// Self-test probe: compile-time and run-time refusals. Not part of a real Application.
import { Refused } from "@vm/sdk";
import type { Ctx } from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";
import { notes } from "./db/schema.ts";

export function typeProbes(ctx: Ctx) {
  // @ts-expect-error payroll is not declared in manifest.ts
  ctx.user().db("payroll");
  // @ts-expect-error devops is declared at read
  ctx.user().db("devops").insert(incidents);
  // @ts-expect-error acting as the Application on a shared Resource
  ctx.app().db("devops");
  // @ts-expect-error unknown column
  ctx.user().db("devops").select({ x: incidents.nope }).from(incidents);
  ctx.user().ownDb().insert(notes).values({ incidentId: 1, author: "a", body: "b" });
}

/** Runs each forbidden operation by going around the types, and reports the refusal reason. */
export async function runtimeProbes(ctx: Ctx): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  const attempt = async (name: string, fn: () => Promise<unknown>) => {
    try {
      await fn();
      out[name] = "allowed";
    } catch (e) {
      out[name] = e instanceof Refused ? e.refusal.reason : `error: ${(e as Error).message.slice(0, 80)}`;
    }
  };
  const anyUser = ctx.user() as any;
  const anyApp = ctx.app() as any;
  await attempt("undeclared", () => anyUser.db("payroll").select().from(incidents));
  await attempt("write-through-read", () => anyUser.db("devops").insert(incidents).values({ serviceId: 1, title: "x", severity: "sev4" }));
  await attempt("app-on-shared", () => (anyApp.db ? anyApp.db("devops").select().from(incidents) : Promise.reject(new Error("no db() on ctx.app()"))));
  return out;
}
