ALTER TABLE "reshaped" DROP COLUMN "gone";
--> statement-breakpoint
ALTER TABLE "reshaped" ADD COLUMN "added" text;
--> statement-breakpoint
ALTER TABLE "reshaped" RENAME COLUMN "before_name" TO "after_name";
--> statement-breakpoint
ALTER TABLE "recast" ALTER COLUMN "code" SET DATA TYPE integer USING "code"::integer;
