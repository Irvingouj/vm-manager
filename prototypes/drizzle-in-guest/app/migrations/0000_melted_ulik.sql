CREATE TABLE "tasks" (
	"id" serial PRIMARY KEY NOT NULL,
	"owner_id" uuid NOT NULL,
	"title" text NOT NULL,
	"done" boolean DEFAULT false NOT NULL,
	"note" text,
	"estimate" numeric(20, 6),
	"big_count" bigint,
	"small_count" bigint,
	"priority" integer,
	"due_at" timestamp,
	"due_at_tz" timestamp with time zone,
	"due_at_string" timestamp,
	"due_date" date,
	"meta" jsonb,
	"meta_json" json,
	"tags" text[],
	"scores" integer[],
	"blob" "bytea"
);
--> statement-breakpoint
CREATE TABLE "users" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"name" text NOT NULL,
	"email" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "users_email_unique" UNIQUE("email")
);
--> statement-breakpoint
ALTER TABLE "tasks" ADD CONSTRAINT "tasks_owner_id_users_id_fk" FOREIGN KEY ("owner_id") REFERENCES "public"."users"("id") ON DELETE no action ON UPDATE no action;