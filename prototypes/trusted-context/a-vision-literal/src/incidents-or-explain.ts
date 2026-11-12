// (c) Handle a refusal and return a useful response.
import { Refused, type TrustedContext } from "@vm/sdk";

export async function incidentsOrExplain(request: Request, ctx: TrustedContext): Promise<Response> {
  try {
    const rows = await ctx.user().db("devops").select("incidents");
    return Response.json(rows);
  } catch (e) {
    if (!(e instanceof Refused)) throw e;
    switch (e.refusal.reason) {
      case "no-user":
        return Response.redirect(new URL("/.vm/sign-in", request.url).toString(), 302);
      case "no-grant":
        return Response.json(
          { error: `You have no ${e.refusal.needed} access to ${e.refusal.resource}. Ask IT to add you to a Group that has it.` },
          { status: 403 },
        );
      default:
        throw e; // a bug in this Application (not-declared etc.): let the host log and answer 500/403
    }
  }
}
