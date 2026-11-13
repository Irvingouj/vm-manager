// The own database's structure. Its migration, migrations/0000_ledger.sql,
// is written by hand to match.
import { integer, pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  author: text("author"),
});

export const counters = pgTable("counters", {
  name: text("name").primaryKey(),
  n: integer("n").notNull(),
});
