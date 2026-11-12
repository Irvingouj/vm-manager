// (c) Handle a refusal and return a useful response.
import { db, Refused, type TrustedContext } from "@vm/sdk";

export async function incidentsOrExplain(request: Request, ctx: TrustedContext): Promise<Response> {
  if (!ctx.user) return Response.redirect(new URL("/.vm/sign-in", request.url).toString(), 302);
  try {
    return Response.json(await db("devops").select(ctx.user, "incidents"));
  } catch (e) {
    if (e instanceof Refused && e.refusal.reason === "no-grant") {
      return Response.json(
        { error: `You have no ${e.refusal.needed} access to ${e.refusal.resource}. Ask IT to add you to a Group that has it.` },
        { status: 403 },
      );
    }
    throw e; // anything else is a bug in this Application: let the host log it
  }
}
