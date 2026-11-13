//! Ordinary migrations still apply (ticket "The Change history is kept out
//! of a Contributor's reach", eighth acceptance criterion: "Ordinary
//! migrations as `drizzle-kit` writes them (create, alter, retype, rename,
//! index, constrain, drop, new schemas, partitions) still apply"; CONTRACT.md,
//! "Identity and Change history in the own database: Change history":
//! every table a migration makes is captured).
//!
//! A realistic sequence of migrations over three Versions, in the form
//! `drizzle-kit` writes them, with rows written between the Versions: an
//! enum, tables with serial and identity keys, foreign keys with `ON DELETE
//! cascade`, a unique constraint, plain, expression and partial indexes;
//! then a column added, renamed, retyped and dropped, an enum value added,
//! a table renamed, a composite primary key; then a new schema and a table
//! moved into it, a partitioned table with partitions, a view, a function
//! and a trigger of the Application's own, and a trusted extension. Each
//! Version is `201`, and afterwards a change of every table through the
//! Application leaves its history row.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::history::{APP, history_after, last_history_id};
use blackbox::out_of_reach::{WRITER, assert_three_roles, start};
use blackbox::own_database::{Migration, Own, assert_ok, own_execute, own_query};
use blackbox::scenario::lines;
use serde_json::Value;

fn v1() -> Migration {
    Migration::new(
        "0000_blog",
        &[
            r#"CREATE TYPE "public"."status" AS ENUM('draft', 'published');"#,
            r#"CREATE TABLE "authors" (
	"id" serial PRIMARY KEY NOT NULL,
	"name" text NOT NULL,
	"email" varchar(255) NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "authors_email_unique" UNIQUE("email")
);"#,
            r#"CREATE TABLE "posts" (
	"id" integer PRIMARY KEY GENERATED ALWAYS AS IDENTITY (sequence name "posts_id_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 2147483647 START WITH 1 CACHE 1),
	"author_id" integer NOT NULL,
	"title" text NOT NULL,
	"status" "status" DEFAULT 'draft' NOT NULL,
	"body" jsonb
);"#,
            r#"ALTER TABLE "posts" ADD CONSTRAINT "posts_author_id_authors_id_fk" FOREIGN KEY ("author_id") REFERENCES "public"."authors"("id") ON DELETE cascade ON UPDATE no action;"#,
            r#"CREATE INDEX "posts_author_idx" ON "posts" USING btree ("author_id");"#,
            r#"CREATE UNIQUE INDEX "authors_lower_email_idx" ON "authors" USING btree (lower("email"));"#,
            r#"CREATE INDEX "posts_published_idx" ON "posts" USING btree ("title") WHERE "posts"."status" = 'published';"#,
        ],
    )
}

fn v2() -> Migration {
    Migration::new(
        "0001_reshape",
        &[
            r#"CREATE TABLE "tags" (
	"id" bigserial PRIMARY KEY NOT NULL,
	"label" text NOT NULL
);"#,
            r#"CREATE TABLE "post_tags" (
	"post_id" integer NOT NULL,
	"tag_id" bigint NOT NULL,
	CONSTRAINT "post_tags_post_id_tag_id_pk" PRIMARY KEY("post_id","tag_id")
);"#,
            r#"ALTER TABLE "posts" ADD COLUMN "views" integer DEFAULT 0 NOT NULL;"#,
            r#"ALTER TABLE "authors" RENAME COLUMN "name" TO "display_name";"#,
            r#"ALTER TABLE "posts" ALTER COLUMN "title" SET DATA TYPE varchar(300);"#,
            r#"ALTER TABLE "posts" ALTER COLUMN "views" SET DATA TYPE bigint;"#,
            r#"ALTER TABLE "posts" DROP COLUMN "body";"#,
            r#"ALTER TYPE "public"."status" ADD VALUE 'archived';"#,
            r#"ALTER TABLE "post_tags" ADD CONSTRAINT "post_tags_post_id_posts_id_fk" FOREIGN KEY ("post_id") REFERENCES "public"."posts"("id") ON DELETE cascade ON UPDATE no action;"#,
            r#"ALTER TABLE "post_tags" ADD CONSTRAINT "post_tags_tag_id_tags_id_fk" FOREIGN KEY ("tag_id") REFERENCES "public"."tags"("id") ON DELETE cascade ON UPDATE no action;"#,
            r#"ALTER TABLE "authors" RENAME TO "writers";"#,
            r#"ALTER TABLE "posts" ADD CONSTRAINT "posts_views_check" CHECK ("posts"."views" >= 0);"#,
        ],
    )
}

fn v3() -> Migration {
    Migration::new(
        "0002_archive_and_events",
        &[
            r#"CREATE SCHEMA "archive";"#,
            r#"ALTER TABLE "tags" SET SCHEMA "archive";"#,
            r#"CREATE TABLE "events" (
	"id" bigserial NOT NULL,
	"at" timestamp with time zone NOT NULL,
	"kind" text NOT NULL,
	CONSTRAINT "events_id_at_pk" PRIMARY KEY("id","at")
) PARTITION BY RANGE ("at");"#,
            r#"CREATE TABLE "events_2026" PARTITION OF "events" FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');"#,
            r#"CREATE TABLE "events_2027" PARTITION OF "events" FOR VALUES FROM ('2027-01-01') TO ('2028-01-01');"#,
            r#"CREATE VIEW "public"."published_posts" AS (select "id", "title" from "posts" where "posts"."status" = 'published');"#,
            r#"ALTER TABLE "posts" ADD COLUMN "updated_at" timestamp with time zone;"#,
            r#"CREATE FUNCTION "touch_updated_at"() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  NEW.updated_at = now();
  RETURN NEW;
END
$$;"#,
            r#"CREATE TRIGGER "posts_touch" BEFORE UPDATE ON "posts" FOR EACH ROW EXECUTE FUNCTION "touch_updated_at"();"#,
            r#"CREATE EXTENSION IF NOT EXISTS "pgcrypto";"#,
            r#"ALTER TABLE "writers" ADD COLUMN "token" text;"#,
            r#"ALTER TABLE "writers" ALTER COLUMN "token" SET DEFAULT encode(gen_random_bytes(8), 'hex');"#,
            r#"UPDATE "posts" SET "views" = "views" + 1;"#,
        ],
    )
}

/// Runs these writes through the Application as [`WRITER`]; each must run.
fn writes(own: &Own, sqls: &[&str]) {
    let calls: Vec<Value> = sqls.iter().map(|s| own_execute("user", s)).collect();
    let (_, answers) = own.run(&User::new(WRITER), APP, &calls);
    for (answer, sql) in answers.iter().zip(sqls) {
        assert_ok(answer, sql);
    }
}

#[test]
fn ordinary_drizzle_kit_migrations_still_apply_and_every_table_they_make_is_captured() {
    let own = start();
    own.create(APP);
    let mut list = vec![v1()];
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "Version 1: {}", sent.describe());
    let db = own.own_database(APP);
    writes(
        &own,
        &[
            "insert into authors (name, email) values ('Ann', 'ann@example.com')",
            "insert into posts (author_id, title, status, body) values ((select id from authors where email = 'ann@example.com'), 'Hello', 'published', '{\"x\": 1}')",
        ],
    );
    list.push(v2());
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "Version 2: {}", sent.describe());
    writes(
        &own,
        &[
            "insert into tags (label) values ('rust')",
            "insert into post_tags (post_id, tag_id) values ((select id from posts limit 1), (select id from tags limit 1))",
        ],
    );
    list.push(v3());
    let sent = own.send(APP, &list);
    assert_eq!(sent.status, 201, "Version 3: {}", sent.describe());
    // Every table made is captured: a change of each leaves its history row.
    let last = last_history_id(&db);
    writes(
        &own,
        &[
            "insert into writers (display_name, email) values ('Bob', 'bob@example.com')",
            "insert into posts (author_id, title, status) values ((select id from writers where email = 'bob@example.com'), 'Second', 'published')",
            "insert into archive.tags (label) values ('postgres')",
            "insert into post_tags (post_id, tag_id) values ((select id from posts where title = 'Second'), (select id from archive.tags where label = 'postgres'))",
            "insert into events (at, kind) values ('2026-05-01T00:00:00Z', 'opened'), ('2027-05-01T00:00:00Z', 'closed')",
            "update posts set title = 'Second, edited' where title = 'Second'",
            "delete from writers where email = 'bob@example.com'",
        ],
    );
    let rows = history_after(&db, last);
    let has = |schema: &str, tables: &[&str], kind: &str| {
        rows.iter().any(|r| {
            r["schema_name"] == schema
                && tables.iter().any(|t| r["table_name"] == *t)
                && r["kind"] == kind
                && r["user_id"] == WRITER
        })
    };
    let expected: [(&str, &[&str], &str); 9] = [
        ("public", &["writers"], "insert"),
        ("public", &["writers"], "delete"),
        ("public", &["posts"], "insert"),
        ("public", &["posts"], "update"),
        ("public", &["posts"], "delete"),
        ("archive", &["tags"], "insert"),
        ("public", &["post_tags"], "insert"),
        ("public", &["post_tags"], "delete"),
        (
            "public",
            &["events", "events_2026", "events_2027"],
            "insert",
        ),
    ];
    let missing: Vec<String> = expected
        .iter()
        .filter(|(schema, tables, kind)| !has(schema, tables, kind))
        .map(|(schema, tables, kind)| format!("{kind} of {schema}.{}", tables.join("|")))
        .collect();
    assert!(
        missing.is_empty(),
        "expected every table the migrations made to be captured; no history row for: {missing:?}; the rows recorded:\n{}",
        lines(&rows)
    );
    let updated = rows
        .iter()
        .find(|r| r["table_name"] == "posts" && r["kind"] == "update")
        .unwrap();
    assert!(
        updated["new"]["updated_at"].is_string(),
        "expected the Application's own trigger's change recorded in the row, got {updated}"
    );
    let (_, answer) = own.one(
        &User::new(WRITER),
        APP,
        &own_query("user", "select count(*)::int4 as n from published_posts"),
    );
    assert_ok(&answer, "the view");
    assert_three_roles(&own, &db, "after three ordinary Versions");
}
