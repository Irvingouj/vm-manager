CREATE TABLE "notes" (
	"id" serial PRIMARY KEY NOT NULL,
	"body" text NOT NULL,
	"author" text
);
--> statement-breakpoint
CREATE TABLE "counters" (
	"name" text PRIMARY KEY NOT NULL,
	"n" integer NOT NULL
);
--> statement-breakpoint
INSERT INTO "counters" VALUES ('hits', 0);
