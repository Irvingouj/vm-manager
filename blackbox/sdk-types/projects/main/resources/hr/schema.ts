import { integer, pgTable, text } from "drizzle-orm/pg-core";

export const employees = pgTable("employees", {
  id: integer("id").primaryKey(),
  name: text("name").notNull(),
  team: text("team"),
});
