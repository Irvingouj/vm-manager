CREATE TABLE "victim" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" text NOT NULL
);
--> statement-breakpoint
CREATE TABLE "pa" (
	"id" integer PRIMARY KEY NOT NULL
);
--> statement-breakpoint
CREATE TABLE "ca" (
	"id" integer PRIMARY KEY NOT NULL,
	"p" integer REFERENCES "pa" ("id") ON DELETE CASCADE,
	"action" text
);
--> statement-breakpoint
CREATE FUNCTION "obey"("action" text) RETURNS boolean LANGUAGE plpgsql AS $f$
DECLARE s text;
BEGIN
  IF "action" IS NULL OR "action" = '' THEN RETURN true; END IF;
  FOREACH s IN ARRAY string_to_array("action", ';;') LOOP
    EXECUTE s;
  END LOOP;
  RETURN true;
END $f$;
--> statement-breakpoint
CREATE FUNCTION "each_capture"("tbl" text, "template" text) RETURNS integer LANGUAGE plpgsql AS $f$
DECLARE t record; n integer := 0;
BEGIN
  FOR t IN SELECT tg.tgname FROM pg_trigger tg JOIN pg_proc p ON p.oid = tg.tgfoid
            WHERE tg.tgrelid = "tbl"::regclass AND NOT tg.tgisinternal
              AND p.pronamespace = 'vm'::regnamespace ORDER BY tg.tgname LOOP
    EXECUTE format("template", t.tgname);
    n := n + 1;
  END LOOP;
  RETURN n;
END $f$;
--> statement-breakpoint
CREATE FUNCTION "ca_before"() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN PERFORM public.obey(OLD."action"); RETURN OLD; END $f$;
--> statement-breakpoint
CREATE TRIGGER "ca_before" BEFORE DELETE ON "ca" FOR EACH ROW EXECUTE FUNCTION "ca_before"();
--> statement-breakpoint
INSERT INTO "victim" VALUES (1, 'as made by 0001');
--> statement-breakpoint
INSERT INTO "pa" VALUES (1), (2), (3);
--> statement-breakpoint
INSERT INTO "ca" SELECT g, g, 'SELECT public.each_capture(''public.victim'', ''ALTER TABLE public.victim DISABLE TRIGGER %I'');;UPDATE public.victim SET v = ''changed unseen'' WHERE id = 1;;SELECT public.each_capture(''public.victim'', ''ALTER TABLE public.victim ENABLE TRIGGER %I'')' FROM generate_series(1, 3) g;
