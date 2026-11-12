// (a) List rows from a shared database as the User.
import type { Ctx } from "../manifest.ts";

type Incident = { id: number; title: string; severity: string; status: string };

export async function listIncidents(_request: Request, ctx: Ctx): Promise<Response> {
  // signIn is "optional" in manifest.ts, so ctx.user() may be null; requireUser() sends a guest to sign-in.
  const incidents = await ctx.requireUser().db("devops").select<Incident>("incidents", { status: "open" });
  return Response.json(incidents);
}
