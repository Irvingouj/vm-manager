// (b) Write to the Application's own database as itself, recording which User did it.
import type { TrustedContext } from "@vm/sdk";

export async function acknowledge(request: Request, ctx: TrustedContext): Promise<Response> {
  const user = ctx.user();
  const body = (await request.json()) as { incidentId: number; note: string };
  await ctx.app().ownDb().insert("acknowledgements", {
    incident_id: body.incidentId,
    note: body.note,
    acknowledged_by: user.id, // for display; the Change history records the User independently
  });
  return new Response(null, { status: 201 });
}
