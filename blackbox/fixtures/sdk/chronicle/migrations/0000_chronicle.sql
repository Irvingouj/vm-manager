CREATE TABLE "tasks" (
	"id" integer PRIMARY KEY NOT NULL,
	"title" text NOT NULL,
	"due_at" timestamp,
	"amount" numeric,
	"meta" jsonb,
	"tags" text[],
	"big" bigint
);
--> statement-breakpoint
CREATE TABLE "order_lines" (
	"order_id" integer NOT NULL,
	"line" integer NOT NULL,
	"item" text NOT NULL,
	CONSTRAINT "order_lines_order_id_line_pk" PRIMARY KEY("order_id","line")
);
--> statement-breakpoint
CREATE TABLE "events" (
	"what" text NOT NULL,
	"n" integer
);
--> statement-breakpoint
CREATE SCHEMA "archive";
--> statement-breakpoint
CREATE TABLE "archive"."notes" (
	"id" integer PRIMARY KEY NOT NULL,
	"body" text NOT NULL
);
--> statement-breakpoint
CREATE TABLE "reshaped" (
	"id" integer PRIMARY KEY NOT NULL,
	"keep" text,
	"gone" text,
	"before_name" text
);
--> statement-breakpoint
CREATE TABLE "recast" (
	"id" integer PRIMARY KEY NOT NULL,
	"code" text
);
