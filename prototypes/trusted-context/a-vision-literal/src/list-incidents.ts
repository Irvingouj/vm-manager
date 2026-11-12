// (a) List rows from a shared database as the User.
import type { TrustedContext } from "@vm/sdk";

type Incident = { id: number; title: string; severity: string; status: string };

export async function listIncidents(_request: Request, ctx: TrustedContext): Promise<Response> {
  const incidents = await ctx.user().db("devops").select<Incident>("incidents", { status: "open" });
  return Response.json(incidents);
}
