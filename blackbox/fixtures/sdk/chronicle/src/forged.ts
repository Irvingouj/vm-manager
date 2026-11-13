// `vm.history` declared by the Application as a Drizzle table of its own
// (CONTRACT.md, "Reading the Change history: Only its own, and only to
// read"). The SDK exports no such table; this one is written by hand.
import { bigint, jsonb, pgSchema, text, timestamp } from "drizzle-orm/pg-core";

export const forgedHistory = pgSchema("vm").table("history", {
  id: bigint("id", { mode: "bigint" }),
  at: timestamp("at", { withTimezone: true }),
  schemaName: text("schema_name"),
  tableName: text("table_name"),
  kind: text("kind"),
  key: jsonb("key"),
  old: jsonb("old"),
  new: jsonb("new"),
  changed: jsonb("changed"),
  userId: text("user_id"),
  application: text("application"),
  actor: text("actor"),
  request: text("request"),
  migration: text("migration"),
});
