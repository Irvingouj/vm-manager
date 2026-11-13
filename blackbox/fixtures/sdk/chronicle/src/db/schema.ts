// The own database's structure, in its present shape. Its migrations,
// migrations/*.sql, are written by hand to match; `reshaped` and `recast`
// end in this shape only after 0002_reshape.
import { bigint, integer, jsonb, numeric, pgSchema, pgTable, primaryKey, text, timestamp } from "drizzle-orm/pg-core";

/** One column of each kind of value; `dueAt` is `due_at` in the database. */
export const tasks = pgTable("tasks", {
  id: integer("id").primaryKey(),
  title: text("title").notNull(),
  dueAt: timestamp("due_at"),
  amount: numeric("amount"),
  meta: jsonb("meta"),
  tags: text("tags").array(),
  big: bigint("big", { mode: "bigint" }),
});

/** A composite primary key, declared on the table, not on its columns. */
export const orderLines = pgTable(
  "order_lines",
  {
    orderId: integer("order_id").notNull(),
    line: integer("line").notNull(),
    item: text("item").notNull(),
  },
  (t) => [primaryKey({ columns: [t.orderId, t.line] })],
);

/** No primary key. */
export const events = pgTable("events", {
  what: text("what").notNull(),
  n: integer("n"),
});

/** A table in a schema other than `public`. */
export const archive = pgSchema("archive");
export const archivedNotes = archive.table("notes", {
  id: integer("id").primaryKey(),
  body: text("body").notNull(),
});

/** Made with `gone` and `before_name`; 0002 drops `gone`, adds `added` and renames `before_name`. */
export const reshaped = pgTable("reshaped", {
  id: integer("id").primaryKey(),
  keep: text("keep"),
  added: text("added"),
  afterName: text("after_name"),
});

/** `code` was text; 0002 made it integer, and an old value ('x1') does not cast. */
export const recast = pgTable("recast", {
  id: integer("id").primaryKey(),
  code: integer("code"),
});
