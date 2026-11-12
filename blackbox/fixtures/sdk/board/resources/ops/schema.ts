// The structure of the shared database the tests register as `ops` (the
// same database as `devops`, with its write login), as much of it as the
// Application writes.
import { sql } from "drizzle-orm";
import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  writtenBy: text("written_by").notNull().default(sql`CURRENT_USER`),
  sessionLogin: text("session_login").notNull().default(sql`SESSION_USER`),
  labelUser: text("label_user"),
  labelApplication: text("label_application"),
  labelRequest: text("label_request"),
});
