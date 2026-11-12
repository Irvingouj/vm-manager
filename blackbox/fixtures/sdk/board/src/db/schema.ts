// The own database's structure. Its migration, migrations/0000_board.sql, is
// written by hand to match (generating it is a later ticket).
import {
  bigint,
  boolean,
  date,
  doublePrecision,
  integer,
  json,
  jsonb,
  numeric,
  pgTable,
  real,
  serial,
  smallint,
  text,
  timestamp,
  uuid,
  varchar,
} from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  author: text("author"),
});

export const counters = pgTable("counters", {
  name: text("name").primaryKey(),
  n: integer("n").notNull(),
});

export const tags = pgTable("tags", {
  name: text("name").primaryKey(),
});

// One column of each type of CONTRACT.md's table "Values".
export const vals = pgTable("vals", {
  id: serial("id").primaryKey(),
  label: text("label").notNull().unique(),
  i4: integer("i4"),
  i2: smallint("i2"),
  big: bigint("big", { mode: "bigint" }),
  bigNum: bigint("big_num", { mode: "number" }),
  num: numeric("num"),
  r4: real("r4"),
  f8: doublePrecision("f8"),
  flag: boolean("flag"),
  t: text("t"),
  vc: varchar("vc", { length: 64 }),
  u: uuid("u"),
  ts: timestamp("ts", { precision: 6 }),
  tstz: timestamp("tstz", { precision: 6, withTimezone: true }),
  tsStr: timestamp("ts_str", { precision: 6, mode: "string" }),
  tstzStr: timestamp("tstz_str", { precision: 6, withTimezone: true, mode: "string" }),
  d: date("d"),
  j: json("j"),
  jb: jsonb("jb"),
  texts: text("texts").array(),
  ints: integer("ints").array(),
});
