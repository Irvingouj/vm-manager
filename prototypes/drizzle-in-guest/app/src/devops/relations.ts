import { relations } from "drizzle-orm/relations";
import { teams, services, deployments } from "./schema";

export const servicesRelations = relations(services, ({one, many}) => ({
	team: one(teams, {
		fields: [services.teamId],
		references: [teams.id]
	}),
	deployments: many(deployments),
}));

export const teamsRelations = relations(teams, ({many}) => ({
	services: many(services),
}));

export const deploymentsRelations = relations(deployments, ({one}) => ({
	service: one(services, {
		fields: [deployments.serviceId],
		references: [services.id]
	}),
}));