// Written by hand, until `vibe pull` writes it: the part of the structure of
// the fixture database `stock` (blackbox/src/structure.rs) that this
// project uses, with the export names and column keys CONTRACT.md promises
// ("Pulling a Resource's structure: The definition"). The tests pull the
// real definition into a copy of this project and type-check the same code
// against it.
import { sql } from "drizzle-orm";
import {
  bigint,
  boolean,
  char,
  customType,
  date,
  doublePrecision,
  foreignKey,
  integer,
  interval,
  json,
  jsonb,
  numeric,
  pgEnum,
  pgMaterializedView,
  pgSchema,
  pgTable,
  pgView,
  primaryKey,
  real,
  serial,
  bigserial,
  smallint,
  smallserial,
  text,
  time,
  timestamp,
  unique,
  uuid,
  varchar,
} from "drizzle-orm/pg-core";

const unsupported = (name: string) => customType<{ data: unknown }>({ dataType: () => name });
const polygon = unsupported("polygon");
const bytea = unsupported("bytea");
const int4range = unsupported("int4range");
const positiveInt = unsupported("positive_int");

export const crmSchema = pgSchema("crm");

export const status = pgEnum("status", ["open", "in progress", "closed"]);
export const crmTier = crmSchema.enum("tier", ["bronze", "silver", "gold"]);

export const sites = pgTable(
  "sites",
  {
    id: integer("id").primaryKey().generatedAlwaysAsIdentity(),
    code: varchar("code", { length: 16 }).notNull(),
    name: text("name").notNull(),
    openedOn: date("opened_on", { mode: "string" }),
    area: polygon("area"),
    active: boolean("active").notNull().default(sql`true`),
  },
  (t) => [unique("sites_code_key").on(t.code)],
);

export const items = pgTable(
  "items",
  {
    id: bigint("id", { mode: "number" }).primaryKey().generatedByDefaultAsIdentity(),
    siteId: integer("site_id").notNull(),
    sku: char("sku", { length: 8 }).notNull(),
    label: text("label").notNull(),
    status: status("status").notNull().default(sql`'open'::status`),
    price: numeric("price", { precision: 10, scale: 2 }),
    weight: real("weight"),
    ratio: doublePrecision("ratio"),
    qty: smallint("qty").notNull().default(sql`0`),
    tags: text("tags").array(),
    grid: integer("grid").array(),
    statuses: status("statuses").array(),
    addedAt: timestamp("added_at", { precision: 3, withTimezone: true, mode: "string" }).notNull().default(sql`now()`),
    checkedAt: timestamp("checked_at", { mode: "string" }),
    opens: time("opens"),
    opensTz: time("opens_tz", { precision: 0, withTimezone: true }),
    shelfLife: interval("shelf_life"),
    ref: uuid("ref"),
    attrs: jsonb("attrs"),
    raw: json("raw"),
    photo: bytea("photo"),
    stocked: int4range("stocked"),
    quantity: positiveInt("quantity"),
    labelUpper: text("label_upper").generatedAlwaysAs(sql`upper(label)`),
  },
  (t) => [
    unique("items_site_sku_key").on(t.siteId, t.sku),
    foreignKey({ name: "items_site_id_fkey", columns: [t.siteId], foreignColumns: [sites.id] })
      .onDelete("cascade")
      .onUpdate("restrict"),
  ],
);

export const movements = pgTable(
  "movements",
  {
    itemId: bigint("item_id", { mode: "number" }).notNull(),
    seq: integer("seq").notNull(),
    serialNo: serial("serial_no").notNull(),
    bigSerial: bigserial("big_serial", { mode: "number" }).notNull(),
    smallSerial: smallserial("small_serial").notNull(),
    amount: integer("amount").notNull(),
  },
  (t) => [
    primaryKey({ name: "movements_pkey", columns: [t.itemId, t.seq] }),
    foreignKey({ name: "movements_item_id_fkey", columns: [t.itemId], foreignColumns: [items.id] })
      .onDelete("no action")
      .onUpdate("cascade"),
  ],
);

export const crmAccounts = crmSchema.table(
  "accounts",
  {
    id: uuid("id").primaryKey().default(sql`gen_random_uuid()`),
    name: text("name").notNull(),
    tier: crmTier("tier").notNull().default(sql`'bronze'::crm.tier`),
    homeSite: integer("home_site"),
  },
  (t) => [
    foreignKey({ name: "accounts_home_site_fkey", columns: [t.homeSite], foreignColumns: [sites.id] }).onDelete(
      "set null",
    ),
  ],
);

export const openItems = pgView("open_items", {
  id: bigint("id", { mode: "number" }),
  siteId: integer("site_id"),
  label: text("label"),
  status: status("status"),
}).existing();

export const siteTotals = pgMaterializedView("site_totals", {
  siteId: integer("site_id"),
  itemCount: bigint("item_count", { mode: "number" }),
}).existing();
