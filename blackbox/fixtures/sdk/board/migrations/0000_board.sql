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
CREATE TABLE "tags" (
	"name" text PRIMARY KEY NOT NULL
);
--> statement-breakpoint
CREATE TABLE "vals" (
	"id" serial PRIMARY KEY NOT NULL,
	"label" text NOT NULL,
	"i4" integer,
	"i2" smallint,
	"big" bigint,
	"big_num" bigint,
	"num" numeric,
	"r4" real,
	"f8" double precision,
	"flag" boolean,
	"t" text,
	"vc" varchar(64),
	"u" uuid,
	"ts" timestamp (6),
	"tstz" timestamp (6) with time zone,
	"ts_str" timestamp (6),
	"tstz_str" timestamp (6) with time zone,
	"d" date,
	"j" json,
	"jb" jsonb,
	"texts" text[],
	"ints" integer[],
	CONSTRAINT "vals_label_unique" UNIQUE("label")
);
--> statement-breakpoint
INSERT INTO "counters" VALUES ('hits', 0);
--> statement-breakpoint
INSERT INTO "vals" ("label", "i4", "i2", "big", "big_num", "num", "r4", "f8", "flag", "t", "vc", "u", "ts", "tstz", "ts_str", "tstz_str", "d", "j", "jb", "texts", "ints") VALUES
  ('every', 2147483647, -32768, 9007199254740993, 9007199254740991, 12345678901234567890.012345678901234567890, 1.5, 3.141592653589793, true,
   'héllo ☃ "quoted" \ back', 'varchar value', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
   '2026-01-02 03:04:05.678412', '2026-01-02 03:04:05.678412+00', '2026-01-02 03:04:05.678412', '2026-01-02 03:04:05.678412+00', '2026-01-02',
   '{"a": [1, "x", null], "b": "123"}', '"123"',
   ARRAY['a,b', 'q"uote', 'back\slash', '{brace}', '', NULL, 'NULL', ' spaced '], ARRAY[1, NULL, -3]);
--> statement-breakpoint
INSERT INTO "vals" ("label") VALUES ('nulls');
--> statement-breakpoint
INSERT INTO "vals" ("label", "j", "jb", "texts", "ints") VALUES
  ('json-number', '123', '123', '{}', '{}'),
  ('json-bool', 'false', 'true', NULL, NULL),
  ('json-null', 'null', 'null', NULL, NULL),
  ('json-nested', '[null, "123", {}]', '{"k": null, "s": "123", "e": []}', NULL, NULL);
