// drizzle-orm's own migrator (node-postgres), for comparison with the
// platform's Rust one: node kit/drizzle-migrate.mjs <url> <folder>
// Prints the bookkeeping rows after migrating.
import pg from "pg";
import { drizzle } from "drizzle-orm/node-postgres";
import { migrate } from "drizzle-orm/node-postgres/migrator";

const [url, folder] = process.argv.slice(2);
const client = new pg.Client({ connectionString: url });
await client.connect();
await migrate(drizzle(client), { migrationsFolder: folder });
const { rows } = await client.query(
  'select hash, created_at::text as created_at from "drizzle"."__drizzle_migrations" order by id',
);
console.log(JSON.stringify(rows));
await client.end();
