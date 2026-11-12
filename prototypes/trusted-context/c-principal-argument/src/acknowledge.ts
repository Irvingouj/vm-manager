// (b) Write to the Application's own database as itself, recording which User did it.
import { ownDb, type TrustedContext } from "@vm/sdk";

export async function acknowledge(request: Request, ctx: TrustedContext): Promise<Response> {
  if (!ctx.user) return new Response("Sign in to acknowledge an incident", { status: 401 });
  const body = (await request.json()) as { incidentId: number; note: string };
  await ownDb().insert(ctx.app, "acknowledgements", {
    incident_id: body.incidentId,
    note: body.note,
    acknowledged_by: ctx.user.id, // for display; the Change history records the User independently
  });
  return new Response(null, { status: 201 });
}
