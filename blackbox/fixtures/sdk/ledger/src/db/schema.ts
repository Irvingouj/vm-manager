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

// migrations/0001_cascade.sql: code a migration leaves behind. Deleting a
// row of `pa` deletes its `ca` row through ON DELETE CASCADE, and the
// `BEFORE DELETE` trigger on `ca` runs, with the migration role's rights,
// what the row's `action` says: here, switch the capture of `victim` off,
// change its row, switch it on again (blackbox/tests/out_of_reach_sdk.rs).
export const victim = pgTable("victim", {
  id: integer("id").primaryKey(),
  v: text("v").notNull(),
});

export const pa = pgTable("pa", {
  id: integer("id").primaryKey(),
});

export const ca = pgTable("ca", {
  id: integer("id").primaryKey(),
  p: integer("p"),
  action: text("action"),
});
