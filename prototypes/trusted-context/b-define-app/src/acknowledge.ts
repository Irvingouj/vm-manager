// (b) Write to the Application's own database as itself, recording which User did it.
import type { Ctx } from "../manifest.ts";

export async function acknowledge(request: Request, ctx: Ctx): Promise<Response> {
  const user = ctx.user();
  if (!user) return new Response("Sign in to acknowledge an incident", { status: 401 });
  const body = (await request.json()) as { incidentId: number; note: string };
  await ctx.app().ownDb().insert("acknowledgements", {
    incident_id: body.incidentId,
    note: body.note,
    acknowledged_by: user.id, // for display; the Change history records the User independently
  });
  return new Response(null, { status: 201 });
}
