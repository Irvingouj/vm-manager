// The structure of the shared database the tests register as `devops`
// (blackbox/src/shared_database.rs), as much of it as the Application reads.
import { boolean, integer, pgTable, smallint, text, timestamp } from "drizzle-orm/pg-core";

export const incidents = pgTable("incidents", {
  id: integer("id").primaryKey(),
  title: text("title").notNull(),
  open: boolean("open").notNull(),
  severity: smallint("severity").notNull(),
  reportedAt: timestamp("reported_at", { withTimezone: true }).notNull(),
});
