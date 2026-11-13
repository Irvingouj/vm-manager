//! The platform database's structure, as numbered steps kept in the
//! program and recorded in the database (table `structure_steps`).
//!
//! At start the program applies every step the database has not had yet,
//! each in a transaction of its own, so a step is applied whole or not at
//! all. Every such transaction first takes a transaction-level advisory
//! lock, so two programs starting at once on one platform database take
//! turns: the second finds the step recorded and goes on. A database that
//! records a step this program does not know was written by a newer
//! program, and start-up fails with a message saying so.
//!
//! Adding a table later is adding a step at the end of [`STEPS`]. A step
//! once released is never changed.

use std::fmt;

use tokio_postgres::Client;

use super::{DbError, Pool};

/// One step of the structure.
#[derive(Debug)]
pub struct Step {
    /// 1, 2, 3, ... in order, no gaps.
    pub number: i32,
    /// What it does, in a few words; recorded with it.
    pub name: &'static str,
    /// Statements run in the step's transaction.
    pub sql: &'static str,
}

/// The structure this program expects, oldest step first.
pub const STEPS: &[Step] = &[
    Step {
        number: 1,
        name: "applications, contributors and versions",
        sql: "
        CREATE TABLE applications (
            id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            name text NOT NULL UNIQUE,
            description text,
            created_by text NOT NULL,
            created_at timestamptz NOT NULL DEFAULT now(),
            -- The number of the serving Version; null before the first.
            serving_version integer
        );
        CREATE TABLE contributors (
            -- The order Contributors were added in.
            id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            application_id bigint NOT NULL REFERENCES applications (id),
            user_id text NOT NULL,
            added_at timestamptz NOT NULL DEFAULT now(),
            UNIQUE (application_id, user_id)
        );
        CREATE TABLE versions (
            application_id bigint NOT NULL REFERENCES applications (id),
            number integer NOT NULL CHECK (number > 0),
            bundle text NOT NULL,
            bundle_sha256 text NOT NULL,
            declared_access jsonb NOT NULL,
            git_commit text NOT NULL,
            created_by text NOT NULL,
            created_at timestamptz NOT NULL DEFAULT now(),
            PRIMARY KEY (application_id, number)
        );
        ALTER TABLE applications
            ADD FOREIGN KEY (id, serving_version) REFERENCES versions (application_id, number);
    ",
    },
    Step {
        number: 2,
        name: "resources, grants and the operation log",
        sql: "
        CREATE TABLE resources (
            id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            name text NOT NULL UNIQUE,
            kind text NOT NULL,
            description text NOT NULL,
            owner text NOT NULL,
            contact text NOT NULL,
            -- Where the database is and its logins, in the form
            -- resources::Sealed writes; read by nothing else.
            connection bytea NOT NULL,
            created_by text NOT NULL,
            created_at timestamptz NOT NULL DEFAULT now()
        );
        -- Rises with every write of a Grant, taken under the row's lock, so
        -- the program's memory keeps the latest whatever order writes
        -- report back in.
        CREATE SEQUENCE grant_revisions;
        CREATE TABLE grants (
            resource_id bigint NOT NULL REFERENCES resources (id),
            group_name text NOT NULL,
            access text NOT NULL CHECK (access IN ('structure', 'read', 'write')),
            revision bigint NOT NULL,
            PRIMARY KEY (resource_id, group_name)
        );
        CREATE TABLE operation_log (
            id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            time timestamptz NOT NULL DEFAULT now(),
            request text,
            user_id text,
            application text NOT NULL,
            version integer NOT NULL,
            resource text NOT NULL,
            operation text NOT NULL,
            -- Without its values (crate::statement).
            statement text NOT NULL,
            outcome text NOT NULL
                CHECK (outcome IN ('allowed', 'refused', 'failed', 'unfinished')),
            reason text,
            error text,
            rows bigint,
            duration_ms bigint
        );
    ",
    },
    Step {
        number: 3,
        name: "own databases, migrations of versions, actors in the operation log",
        sql: "
        -- An Application's own database (crate::own_db): its roles'
        -- passwords, chosen once, before the roles exist; `ready` once the
        -- database and its privileges are set up. Read by nothing else.
        CREATE TABLE own_databases (
            application_id bigint PRIMARY KEY REFERENCES applications (id),
            owner_password text NOT NULL,
            runtime_password text NOT NULL,
            ready boolean NOT NULL DEFAULT false,
            created_at timestamptz NOT NULL DEFAULT now()
        );
        -- A Version's migrations, as its record shows them: [{name, sha256}].
        ALTER TABLE versions ADD COLUMN migrations jsonb NOT NULL DEFAULT '[]';
        ALTER TABLE operation_log
            ALTER COLUMN resource DROP NOT NULL,
            ADD COLUMN actor text NOT NULL DEFAULT 'user'
                CHECK (actor IN ('user', 'application')),
            ADD COLUMN own_database boolean NOT NULL DEFAULT false;
        -- Only the platform's role connects to the platform database, not
        -- the roles of Applications' own databases. Without effect (a
        -- warning) when the platform's role does not own it.
        DO $$ BEGIN
            EXECUTE format('REVOKE ALL ON DATABASE %I FROM PUBLIC', current_database());
        END $$;
    ",
    },
    Step {
        number: 4,
        name: "transactions in the operation log",
        sql: "
        -- The id of the transaction a statement ran in, or that a `begin`,
        -- `commit` or `rollback` was of; null otherwise. The id Application
        -- code holds, which works only inside the request that began it.
        ALTER TABLE operation_log ADD COLUMN transaction_id text;
    ",
    },
    Step {
        number: 5,
        name: "the platform's own role of each own database",
        sql: "
        -- Three roles per own database (ADR 0017): the platform's own role
        -- for it, whose password is kept here; the migration role, which
        -- has no password kept anywhere (a fresh one each time it is let in
        -- to apply migrations); the runtime role as before. `form`: the
        -- form of the platform's part of the own database the program last
        -- brought it to (crate::own_db); 1 is the form before this step.
        ALTER TABLE own_databases
            ADD COLUMN platform_password text,
            ADD COLUMN form integer NOT NULL DEFAULT 1,
            ALTER COLUMN owner_password DROP NOT NULL;
    ",
    },
    Step {
        number: 6,
        name: "what each own database's structure and capture were",
        sql: "
        -- What the structure and the capture of each own database's tables
        -- were when the last Version's migrations ended, or when the
        -- platform last compared and put the capture back (crate::own_db;
        -- blackbox/CONTRACT.md, \"What something else changed\"): a digest
        -- per table, {\"<schema>.<table>\": \"<hex>\"}. `pending`: what they
        -- are once the migrations being applied are committed, written just
        -- before that commit; `pending_record` the digest of the own
        -- database's record of migrations with them, by which the next
        -- comparison knows whether the commit was made.
        CREATE TABLE own_database_states (
            application_id bigint PRIMARY KEY REFERENCES applications (id),
            tables jsonb NOT NULL,
            pending jsonb,
            pending_record text
        );
        -- The platform's own entries: what it found changed outside its
        -- transactions (`capture-check`).
        ALTER TABLE operation_log
            DROP CONSTRAINT operation_log_actor_check,
            ADD CONSTRAINT operation_log_actor_check
                CHECK (actor IN ('user', 'application', 'platform'));
    ",
    },
];

/// The advisory lock every step's transaction takes first: "vm steps".
const LOCK: i64 = 0x766d_5f73_7465_7073;

/// Why the structure could not be brought up to date.
#[derive(Debug)]
pub enum StepsError {
    /// The database records a step this program does not know.
    Newer {
        database: i32,
        program: i32,
    },
    Database(DbError),
}

impl fmt::Display for StepsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newer { database, program } => write!(
                f,
                "it was written by a newer program (its structure is at step {database}; this program knows steps up to {program}): run that newer program, or this one against another platform database"
            ),
            Self::Database(error) => {
                write!(f, "its structure could not be brought up to date: {error}")
            }
        }
    }
}

impl From<DbError> for StepsError {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

/// Applies the steps of `steps` the database has not had, each in its own
/// transaction. Returns the numbers of the steps this call applied.
pub async fn bring_up_to_date(pool: &Pool, steps: &[Step]) -> Result<Vec<i32>, StepsError> {
    let known = i32::try_from(steps.len()).unwrap_or(i32::MAX);
    let mut applied = Vec::new();
    loop {
        let outcome = pool
            .run(async |client: &mut Client| next_step(client, steps, known).await)
            .await?;
        match outcome {
            Next::Applied(number) => applied.push(number),
            Next::UpToDate => return Ok(applied),
            Next::Newer(database) => {
                return Err(StepsError::Newer {
                    database,
                    program: known,
                });
            }
        }
    }
}

enum Next {
    Applied(i32),
    UpToDate,
    Newer(i32),
}

/// In one transaction, under the lock: applies the first step the
/// database has not had, if any.
async fn next_step(client: &mut Client, steps: &[Step], known: i32) -> Result<Next, DbError> {
    let transaction = client.transaction().await.map_err(DbError::from)?;
    transaction
        .execute("SELECT pg_advisory_xact_lock($1)", &[&LOCK])
        .await
        .map_err(DbError::from)?;
    transaction
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS structure_steps (
                number integer PRIMARY KEY,
                name text NOT NULL,
                applied_at timestamptz NOT NULL DEFAULT now()
            )",
        )
        .await
        .map_err(DbError::from)?;
    let newest: i32 = transaction
        .query_one("SELECT coalesce(max(number), 0) FROM structure_steps", &[])
        .await
        .and_then(|row| row.try_get(0))
        .map_err(DbError::from)?;
    let next = if newest > known {
        Next::Newer(newest)
    } else if let Some(step) = steps.iter().find(|s| s.number == newest + 1) {
        transaction
            .batch_execute(step.sql)
            .await
            .map_err(|e| DbError::from(e).context(&format!("step {}", step.number)))?;
        transaction
            .execute(
                "INSERT INTO structure_steps (number, name) VALUES ($1, $2)",
                &[&step.number, &step.name],
            )
            .await
            .map_err(DbError::from)?;
        Next::Applied(step.number)
    } else {
        Next::UpToDate
    };
    transaction.commit().await.map_err(DbError::from)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::platform_db::{PlatformDatabase, testing};

    #[test]
    fn the_steps_are_numbered_from_one_without_gaps() {
        for (index, step) in STEPS.iter().enumerate() {
            assert_eq!(usize::try_from(step.number).unwrap(), index + 1, "{step:?}");
        }
    }

    const OLDER: &[Step] = &[Step {
        number: 1,
        name: "notes",
        sql: "CREATE TABLE notes (id integer PRIMARY KEY, text text NOT NULL)",
    }];

    const NEWER: &[Step] = &[
        Step {
            number: 1,
            name: "notes",
            sql: "CREATE TABLE notes (id integer PRIMARY KEY, text text NOT NULL)",
        },
        Step {
            number: 2,
            name: "notes have authors",
            sql: "ALTER TABLE notes ADD COLUMN author text NOT NULL DEFAULT 'nobody'",
        },
    ];

    async fn recorded(database: &PlatformDatabase) -> Vec<i32> {
        database
            .pool()
            .run(async |c: &mut Client| {
                let rows = c
                    .query("SELECT number FROM structure_steps ORDER BY number", &[])
                    .await
                    .map_err(DbError::from)?;
                Ok(rows.iter().map(|r| r.get::<_, i32>(0)).collect())
            })
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fresh_database_gets_every_step_once_and_a_second_start_applies_none() {
        let address = testing::fresh_address();
        let database = PlatformDatabase::connect(&address, &testing::limits())
            .await
            .unwrap();
        assert_eq!(
            bring_up_to_date(database.pool(), NEWER).await.unwrap(),
            vec![1, 2]
        );
        assert_eq!(
            bring_up_to_date(database.pool(), NEWER).await.unwrap(),
            Vec::<i32>::new()
        );
        assert_eq!(recorded(&database).await, vec![1, 2]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_newer_program_brings_an_older_database_up_to_date_in_place() {
        let address = testing::fresh_address();
        let database = PlatformDatabase::connect(&address, &testing::limits())
            .await
            .unwrap();
        assert_eq!(
            bring_up_to_date(database.pool(), OLDER).await.unwrap(),
            vec![1]
        );
        database
            .pool()
            .run(async |c: &mut Client| {
                c.execute("INSERT INTO notes (id, text) VALUES (1, 'kept')", &[])
                    .await
                    .map_err(DbError::from)
            })
            .await
            .unwrap();
        assert_eq!(
            bring_up_to_date(database.pool(), NEWER).await.unwrap(),
            vec![2]
        );
        let row: (String, String) = database
            .pool()
            .run(async |c: &mut Client| {
                let row = c
                    .query_one("SELECT text, author FROM notes WHERE id = 1", &[])
                    .await
                    .map_err(DbError::from)?;
                Ok((row.get(0), row.get(1)))
            })
            .await
            .unwrap();
        assert_eq!(row, ("kept".to_owned(), "nobody".to_owned()));
        assert_eq!(recorded(&database).await, vec![1, 2]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_older_program_refuses_a_database_a_newer_one_wrote() {
        let address = testing::fresh_address();
        let database = PlatformDatabase::connect(&address, &testing::limits())
            .await
            .unwrap();
        bring_up_to_date(database.pool(), NEWER).await.unwrap();
        let error = bring_up_to_date(database.pool(), OLDER).await.unwrap_err();
        assert!(
            matches!(
                error,
                StepsError::Newer {
                    database: 2,
                    program: 1
                }
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("newer program"), "{error}");
        assert_eq!(recorded(&database).await, vec![1, 2]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn programs_starting_at_once_apply_each_step_exactly_once() {
        let address = testing::fresh_address();
        let first = PlatformDatabase::connect(&address, &testing::limits())
            .await
            .unwrap();
        let first = Arc::new(first);
        let mut starts = tokio::task::JoinSet::new();
        for _ in 0..6 {
            let database = Arc::new(
                PlatformDatabase::connect(&address, &testing::limits())
                    .await
                    .unwrap(),
            );
            starts.spawn(async move { bring_up_to_date(database.pool(), NEWER).await });
        }
        let mut applied = Vec::new();
        while let Some(started) = starts.join_next().await {
            applied.extend(started.unwrap().unwrap());
        }
        applied.sort_unstable();
        assert_eq!(
            applied,
            vec![1, 2],
            "each step applied by exactly one program"
        );
        assert_eq!(recorded(&first).await, vec![1, 2]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opening_a_database_a_newer_program_wrote_fails_saying_so() {
        let address = testing::fresh_address();
        let database = PlatformDatabase::connect(&address, &testing::limits())
            .await
            .unwrap();
        let beyond = i32::try_from(STEPS.len()).unwrap() + 1;
        let mut ahead: Vec<Step> = Vec::new();
        for step in STEPS {
            ahead.push(Step { ..*step });
        }
        ahead.push(Step {
            number: beyond,
            name: "from the future",
            sql: "CREATE TABLE from_the_future (id integer)",
        });
        bring_up_to_date(database.pool(), &ahead).await.unwrap();
        drop(database);
        let error = PlatformDatabase::open(&address, &testing::limits())
            .await
            .err()
            .expect("opening must fail");
        assert!(error.contains("newer program"), "{error}");
        assert!(error.contains(address.database()), "{error}");
    }
}
