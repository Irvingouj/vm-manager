// The structure of this Application's own database, in Drizzle.
// After changing it, run `npx vm db generate` to write a migration to migrations/.
//
// Example:
//
//   import { pgTable, serial, text, timestamp } from "drizzle-orm/pg-core";
//
//   export const notes = pgTable("notes", {
//     id: serial("id").primaryKey(),
//     author: text("author").notNull(),
//     body: text("body").notNull(),
//     createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
//   });

export {};
