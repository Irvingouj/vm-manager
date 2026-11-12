//! The platform applies an Application's drizzle-kit migrations itself, with
//! a privileged role, without Node and without drizzle-kit: read
//! `meta/_journal.json`, run each `<tag>.sql` split on
//! `--> statement-breakpoint`, and keep the same bookkeeping table as
//! drizzle-orm's own migrator (`drizzle.__drizzle_migrations`: sha256 of the
//! file, the journal's `when`), so either tool sees the other's work.
//!
//! Mirrors `PgDialect.migrate` in drizzle-orm 0.45.4
//! (`node_modules/drizzle-orm/pg-core/dialect.js`) and `readMigrationFiles`
//! (`node_modules/drizzle-orm/migrator.js`): a migration is applied when its
//! `when` is later than the last applied `created_at`; all pending ones run in
//! one transaction.

use std::path::Path;

use sha2::{Digest, Sha256};
use tokio_postgres::NoTls;

pub async fn apply(conninfo: &str, folder: &Path) -> Result<(), String> {
    let journal: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join("meta/_journal.json")).map_err(|e| format!("journal: {e}"))?,
    )
    .map_err(|e| format!("journal: {e}"))?;

    let (mut client, connection) = tokio_postgres::connect(conninfo, NoTls).await.map_err(|e| e.to_string())?;
    tokio::spawn(connection);

    client
        .batch_execute(
            "CREATE SCHEMA IF NOT EXISTS \"drizzle\";
             CREATE TABLE IF NOT EXISTS \"drizzle\".\"__drizzle_migrations\" (
                 id SERIAL PRIMARY KEY, hash text NOT NULL, created_at bigint)",
        )
        .await
        .map_err(|e| e.to_string())?;
    let last: Option<i64> = client
        .query_opt(
            "select created_at from \"drizzle\".\"__drizzle_migrations\" order by created_at desc limit 1",
            &[],
        )
        .await
        .map_err(|e| e.to_string())?
        .and_then(|r| r.get(0));

    let tx = client.transaction().await.map_err(|e| e.to_string())?;
    let mut applied = 0;
    for entry in journal["entries"].as_array().ok_or("journal has no entries")? {
        let tag = entry["tag"].as_str().ok_or("entry without tag")?;
        let when = entry["when"].as_i64().ok_or("entry without when")?;
        if last.is_some_and(|last| last >= when) {
            continue;
        }
        let file = std::fs::read_to_string(folder.join(format!("{tag}.sql"))).map_err(|e| format!("{tag}: {e}"))?;
        for stmt in file.split("--> statement-breakpoint") {
            if stmt.trim().is_empty() {
                continue;
            }
            tx.batch_execute(stmt).await.map_err(|e| format!("{tag}: {e}: {stmt}"))?;
        }
        let hash = hex::encode(Sha256::digest(file.as_bytes()));
        tx.execute(
            "insert into \"drizzle\".\"__drizzle_migrations\" (\"hash\", \"created_at\") values ($1, $2)",
            &[&hash, &when],
        )
        .await
        .map_err(|e| e.to_string())?;
        eprintln!("applied {tag} ({hash})");
        applied += 1;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    eprintln!("migrate: {applied} applied, last before was {last:?}");
    Ok(())
}
