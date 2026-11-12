import { pgTable, unique, serial, text, index, foreignKey, check, uuid, integer, jsonb, numeric, timestamp, pgPolicy, bigserial, boolean, pgView, bigint, pgEnum } from "drizzle-orm/pg-core"
import { sql } from "drizzle-orm"

export const serviceTier = pgEnum("service_tier", ['gold', 'silver', 'bronze'])


export const teams = pgTable("teams", {
	id: serial().primaryKey().notNull(),
	name: text().notNull(),
}, (table) => [
	unique("teams_name_key").on(table.name),
]);

export const services = pgTable("services", {
	id: uuid().defaultRandom().primaryKey().notNull(),
	name: text().notNull(),
	teamId: integer("team_id").notNull(),
	tier: serviceTier().default('bronze').notNull(),
	config: jsonb().default({}).notNull(),
	tags: text().array().default([""]).notNull(),
	monthlyCost: numeric("monthly_cost", { precision: 10, scale:  2 }),
	createdAt: timestamp("created_at", { withTimezone: true, mode: 'string' }).defaultNow().notNull(),
}, (table) => [
	index("services_team_idx").using("btree", table.teamId.asc().nullsLast().op("int4_ops")),
	foreignKey({
			columns: [table.teamId],
			foreignColumns: [teams.id],
			name: "services_team_id_fkey"
		}),
	unique("services_name_key").on(table.name),
	check("services_monthly_cost_check", sql`monthly_cost >= (0)::numeric`),
]);

export const deployments = pgTable("deployments", {
	id: bigserial({ mode: "bigint" }).primaryKey().notNull(),
	serviceId: uuid("service_id").notNull(),
	version: text().notNull(),
	deployedAt: timestamp("deployed_at", { withTimezone: true, mode: 'string' }).defaultNow().notNull(),
	ok: boolean().notNull(),
}, (table) => [
	foreignKey({
			columns: [table.serviceId],
			foreignColumns: [services.id],
			name: "deployments_service_id_fkey"
		}).onDelete("cascade"),
	pgPolicy("deployments_read", { as: "permissive", for: "select", to: ["public"], using: sql`true` }),
]);
export const serviceSummary = pgView("service_summary", {	name: text(),
	team: text(),
	// You can use { mode: "bigint" } if numbers are exceeding js number limitations
	deployments: bigint({ mode: "number" }),
}).as(sql`SELECT s.name, t.name AS team, count(d.id) AS deployments FROM services s JOIN teams t ON t.id = s.team_id LEFT JOIN deployments d ON d.service_id = s.id GROUP BY s.name, t.name`);