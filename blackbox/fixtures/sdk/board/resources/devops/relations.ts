import { relations } from "drizzle-orm";
import { services, teams } from "./schema.ts";

export const teamsRelations = relations(teams, ({ many }) => ({
  services: many(services),
}));

export const servicesRelations = relations(services, ({ one }) => ({
  team: one(teams, { fields: [services.teamId], references: [teams.id] }),
}));
