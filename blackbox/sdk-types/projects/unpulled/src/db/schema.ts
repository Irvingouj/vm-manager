import { integer, pgTable, serial, text, timestamp } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  incidentId: integer("incident_id").notNull(),
  body: text("body").notNull(),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
});

export const counters = pgTable("counters", {
  name: text("name").primaryKey(),
  n: integer("n").notNull(),
});
