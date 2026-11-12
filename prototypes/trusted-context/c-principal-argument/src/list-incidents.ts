// (a) List rows from a shared database as the User.
import { db, type TrustedContext } from "@vm/sdk";

type Incident = { id: number; title: string; severity: string; status: string };

export async function listIncidents(_request: Request, ctx: TrustedContext): Promise<Response> {
  if (!ctx.user) return new Response("Sign in to see incidents", { status: 401 });
  const incidents = await db("devops").select<Incident>(ctx.user, "incidents", { status: "open" });
  return Response.json(incidents);
}
