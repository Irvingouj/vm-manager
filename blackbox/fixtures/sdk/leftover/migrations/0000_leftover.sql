CREATE TABLE "notes" (
	"id" serial PRIMARY KEY NOT NULL,
	"body" text NOT NULL
);
--> statement-breakpoint
INSERT INTO "notes" ("body") VALUES ('seeded');
