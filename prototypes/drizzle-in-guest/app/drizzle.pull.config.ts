import { defineConfig } from "drizzle-kit";

// A shared database: pull its structure as a read-only typed definition.
// PULL_ROLE picks the role drizzle-kit connects as; PULL_OUT the folder.
export default defineConfig({
  dialect: "postgresql",
  out: process.env.PULL_OUT ?? "./src/devops",
  dbCredentials: {
    url: `postgres://${process.env.PULL_ROLE ?? "devops_reader"}:spike@127.0.0.1:${process.env.PG_PORT ?? "55491"}/devops`,
  },
  schemaFilter: (process.env.PULL_SCHEMAS ?? "public").split(","),
});
