// (c) Handle a refusal and return a useful response.
import { Refused } from "@vm/sdk";
import type { Ctx } from "../manifest.ts";

export async function incidentsOrExplain(request: Request, ctx: Ctx): Promise<Response> {
  const user = ctx.user();
  if (!user) return Response.redirect(new URL("/.vm/sign-in", request.url).toString(), 302);
  try {
    return Response.json(await user.db("devops").select("incidents"));
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
