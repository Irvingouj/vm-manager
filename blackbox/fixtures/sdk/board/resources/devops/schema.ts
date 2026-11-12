// The structure of the shared database the tests register as `devops`
// (blackbox/src/shared_database.rs and blackbox/src/sdk.rs), written by hand
// as `vm resources pull` will write it.
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

export const incidents = pgTable("incidents", {
  id: integer("id").primaryKey(),
  title: text("title").notNull(),
  open: boolean("open").notNull(),
  severity: smallint("severity").notNull(),
  reportedAt: timestamp("reported_at", { withTimezone: true }).notNull(),
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
  tier: smallint("tier").notNull(),
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
