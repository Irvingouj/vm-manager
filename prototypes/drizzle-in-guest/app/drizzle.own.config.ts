import { defineConfig } from "drizzle-kit";

// The Application's own database: generate migrations from the TypeScript
// schema. `generate` needs no database connection.
export default defineConfig({
  dialect: "postgresql",
  schema: process.env.SCHEMA ?? "./src/own/schema.ts",
  out: process.env.OUT ?? "./migrations",
});
