import { bigint, boolean, integer, jsonb, numeric, pgTable, smallint, text, timestamp } from "drizzle-orm/pg-core";

export const incidents = pgTable("incidents", {
  id: integer("id").primaryKey(),
  title: text("title").notNull(),
  open: boolean("open").notNull(),
  severity: smallint("severity").notNull(),
  reportedAt: timestamp("reported_at", { withTimezone: true }).notNull(),
  cost: numeric("cost"),
  views: bigint("views", { mode: "bigint" }),
  details: jsonb("details"),
});

export const teams = pgTable("teams", {
  id: integer("id").primaryKey(),
  name: text("name").notNull(),
});

export const services = pgTable("services", {
  id: integer("id").primaryKey(),
  name: text("name").notNull(),
  teamId: integer("team_id")
    .notNull()
    .references(() => teams.id),
});
