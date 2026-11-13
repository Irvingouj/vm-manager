INSERT INTO "reshaped" VALUES (1, 'k1', 'g1', 'b1');
--> statement-breakpoint
UPDATE "reshaped" SET "keep" = 'k2' WHERE "id" = 1;
--> statement-breakpoint
UPDATE "reshaped" SET "gone" = 'g2' WHERE "id" = 1;
--> statement-breakpoint
INSERT INTO "recast" VALUES (1, 'x1');
--> statement-breakpoint
UPDATE "recast" SET "code" = '5' WHERE "id" = 1;
