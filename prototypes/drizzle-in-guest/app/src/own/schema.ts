// The Application's own database, as a Builder's Coding agent would write it.
// drizzle-kit generates the migrations from this file; the platform applies
// them at deploy.
import { relations } from "drizzle-orm";
import {
  bigint,
  boolean,
  customType,
  date,
  integer,
  json,
  jsonb,
  numeric,
  pgTable,
  serial,
  text,
  timestamp,
  uuid,
} from "drizzle-orm/pg-core";

// drizzle-orm 0.45.4 has no bytea column; a custom type is the documented way.
// The host sends bytea as Postgres hex text (\x...).
export const bytea = customType<{ data: Uint8Array; driverData: string }>({
  dataType() {
    return "bytea";
  },
  toDriver(value) {
    let hex = "\\x";
    for (const b of value) hex += b.toString(16).padStart(2, "0");
    return hex;
  },
  fromDriver(value) {
    const hex = value.startsWith("\\x") ? value.slice(2) : value;
    const out = new Uint8Array(hex.length / 2);
    for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
    return out;
  },
});

export const users = pgTable("users", {
  id: uuid("id").primaryKey().defaultRandom(),
  name: text("name").notNull(),
  email: text("email").notNull().unique(),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
});

export const tasks = pgTable("tasks", {
  id: serial("id").primaryKey(),
  ownerId: uuid("owner_id")
    .notNull()
    .references(() => users.id),
  title: text("title").notNull(),
  done: boolean("done").notNull().default(false),
  note: text("note"),
  estimate: numeric("estimate", { precision: 20, scale: 6 }),
  bigCount: bigint("big_count", { mode: "bigint" }),
  smallCount: bigint("small_count", { mode: "number" }),
  priority: integer("priority"),
  dueAt: timestamp("due_at"),
  dueAtTz: timestamp("due_at_tz", { withTimezone: true }),
  dueAtString: timestamp("due_at_string", { mode: "string" }),
  dueDate: date("due_date"),
  meta: jsonb("meta").$type<{ labels: string[]; weight?: number }>(),
  metaJson: json("meta_json"),
  tags: text("tags").array(),
  scores: integer("scores").array(),
  blob: bytea("blob"),
});

export const usersRelations = relations(users, ({ many }) => ({ tasks: many(tasks) }));
export const tasksRelations = relations(tasks, ({ one }) => ({
  owner: one(users, { fields: [tasks.ownerId], references: [users.id] }),
}));
