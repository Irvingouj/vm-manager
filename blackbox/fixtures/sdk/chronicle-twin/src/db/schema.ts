import { integer, pgTable, text } from "drizzle-orm/pg-core";

export const tasks = pgTable("tasks", {
  id: integer("id").primaryKey(),
  title: text("title").notNull(),
});
