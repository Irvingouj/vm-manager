//! What the ticket "Resources and Grants in full" (stage one) seals under the
//! installation's key (blackbox/CONTRACT.md, "Resources and Grants in full:
//! What is sealed, and under which key" and "At start"; ADR 0018): a shared
//! database's connection, and the role passwords of an Application's own
//! database.
//!
//! [`Vault`] holds the two keys of the installation's key, one for each use
//! (crate::oidc::seal): [`Use::ResourceConnection`] and
//! [`Use::OwnDatabasePassword`]. Nothing here has a `Debug` or a `Display`:
//! a connection, a password and the key are values that do not print.
//!
//! **Bound to its row.** A Resource's connection is sealed with
//! [`resource_connection_bound`] as the associated data: a fixed label, the
//! Resource's id and its name. An own database's password is sealed with
//! [`own_database_password_bound`]: a fixed label, the Application's id and
//! which password it is. So a value copied to another row, or left in a row
//! whose id or name was changed, does not open.
//!
//! **Two forms, told apart without a key.** The stored form of a Resource's
//! connection is [`crate::oidc::seal::FORM`] first (a `bytea`); the form of
//! before this ticket is the connection's JSON, beginning with `{`. An own
//! database's password is stored in its `text` column as
//! [`OWN_FORM_PREFIX`] followed by the sealed bytes in base64; the form of
//! before is 64 hex digits. So the program tells what it must seal at the
//! first start (a clear row) from what another key does not open (a sealed
//! row), and never seals a sealed row again.
//!
//! **Sealing a platform database from before.** [`Vault::seal_old`] finds
//! the rows that exist and, one row at a time and each in a transaction of
//! its own, takes the row's lock (`SELECT ... FOR UPDATE`), reads what is
//! stored under that lock, seals a clear value in the program and writes
//! only the sealed bytes. No statement it sends carries a clear value, as a
//! parameter or in its text. A lock another session holds is waited for at
//! most [`ROW_LOCK_WAIT`] in the database's own `lock_timeout`, so no
//! backend is left waiting: the sealing goes on with the next row and comes
//! back to it while its time lasts. It is bounded by [`SEALING_BOUND`],
//! finding the rows included; a row left in clear (out of time, or its
//! sealing failed) is left as it was, counted in
//! `credentials-sealing-incomplete`, and is not served until a later start
//! seals it.

use std::collections::VecDeque;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use tokio::time::Instant;
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;

use crate::log;
use crate::oidc::key::InstallationKey;
use crate::oidc::seal::{self, Sealer, Use};
use crate::oidc::secret::digest;
use crate::platform_db::{DbError, PlatformDatabase};

/// The longest sealing a platform database from before may take: before
/// `listening`, for 1,000 Resources and 1,000 own databases, finding the
/// rows included (blackbox/CONTRACT.md, "Resources and Grants in full:
/// Bounds").
pub const SEALING_BOUND: Duration = Duration::from_secs(15);

/// How long one row's transaction waits for another session's lock, in the
/// database's own `lock_timeout` (so that its backend stops waiting, not
/// only the program): the sealing goes on with the next row and comes back
/// to this one while its time lasts (blackbox/CONTRACT.md, "Resources and
/// Grants in full: At start").
const ROW_LOCK_WAIT: &str = "1s";

/// What an own database's password stored in its `text` column begins with:
/// the sealed form. What was stored before this ticket (64 hex digits) does
/// not.
pub const OWN_FORM_PREFIX: &str = "vm-sealed-1:";

/// The installation's key, as the two keys this ticket uses are made from.
/// Never printed; no `Debug`, no `Display`.
pub struct Vault {
    resources: Sealer,
    own_databases: Sealer,
}

impl Vault {
    /// The two keys, each derived from `key` for its use alone. The error
    /// is a message for the operator, never the key.
    pub fn new(key: &InstallationKey) -> Result<Self, String> {
        Ok(Self {
            resources: Sealer::new(key, Use::ResourceConnection)?,
            own_databases: Sealer::new(key, Use::OwnDatabasePassword)?,
        })
    }

    /// The key a Resource's connection is sealed with.
    pub fn resource_sealer(&self) -> &Sealer {
        &self.resources
    }

    /// The key an own database's password is sealed with.
    pub fn own_database_sealer(&self) -> &Sealer {
        &self.own_databases
    }

    /// Seals every clear value a platform database from before this ticket
    /// still holds, one row at a time and each in a transaction of its own,
    /// within [`SEALING_BOUND`], finding the rows included. A row another
    /// session holds is not waited for longer than [`ROW_LOCK_WAIT`]: the
    /// sealing goes on with the next and comes back to it while its time
    /// lasts. A row left in clear — out of time, or its sealing failed — is
    /// left as it was and counted in `credentials-sealing-incomplete`;
    /// `credentials-sealed` counts what this start sealed.
    pub async fn seal_old(&self, database: &PlatformDatabase) -> Sealed {
        let deadline = Instant::now() + SEALING_BOUND;
        let mut progress = Progress::default();
        // The two listings at once, and each kind's rows sealed as soon as
        // its listing arrives: a listing another session holds (or one that
        // fails) does not keep the other kind from being sealed.
        let mut list_resources = std::pin::pin!(resource_ids(database));
        let mut list_own = std::pin::pin!(own_database_ids(database));
        let mut resources_listed = false;
        let mut own_listed = false;
        while !resources_listed || !own_listed {
            let found = tokio::select! {
                found = &mut list_resources, if !resources_listed => {
                    resources_listed = true;
                    Some((Kind::Resources, found))
                }
                found = &mut list_own, if !own_listed => {
                    own_listed = true;
                    Some((Kind::OwnDatabases, found))
                }
                _ = tokio::time::sleep_until(deadline) => None,
            };
            let Some((kind, found)) = found else {
                break;
            };
            if let Ok(ids) = found {
                *progress.left.of(kind) = Some(0);
                self.seal_rows(database, kind, ids, deadline, &mut progress)
                    .await;
            }
        }
        // Rows another session held: one of each kind at a turn, while the
        // time lasts; then whatever is left is left as it was.
        loop {
            let mut did_work = false;
            for kind in [Kind::Resources, Kind::OwnDatabases] {
                let Some(position) = progress.retries.iter().position(|(held, _)| *held == kind)
                else {
                    continue;
                };
                let Some((held, id)) = progress.retries.remove(position) else {
                    continue;
                };
                did_work = true;
                if Instant::now() >= deadline {
                    *progress.left.of(held) = progress.left.of(held).map(|n| n.saturating_add(1));
                    continue;
                }
                match tokio::time::timeout_at(deadline, self.seal_one(database, held, id)).await {
                    Ok(Ok(Attempted::AlreadySealed)) => {}
                    Ok(Ok(Attempted::Held)) => progress.retries.push_back((held, id)),
                    Ok(Ok(Attempted::Sealed)) => progress.sealed_of(held),
                    Ok(Err(_)) | Err(_) => {
                        *progress.left.of(held) =
                            progress.left.of(held).map(|n| n.saturating_add(1));
                    }
                }
            }
            if !did_work {
                break;
            }
        }
        // Rows another session still held when the time ran out.
        for (held, _) in progress.retries {
            *progress.left.of(held) = progress.left.of(held).map(|n| n.saturating_add(1));
        }
        finish(progress.sealed, progress.left)
    }

    /// Seals the rows `ids` of one kind, one at a time and each in its own
    /// transaction; a row another session holds is put in the retries.
    async fn seal_rows(
        &self,
        database: &PlatformDatabase,
        kind: Kind,
        ids: Vec<i64>,
        deadline: Instant,
        progress: &mut Progress,
    ) {
        for id in ids {
            if Instant::now() >= deadline {
                *progress.left.of(kind) = progress.left.of(kind).map(|n| n.saturating_add(1));
                continue;
            }
            match tokio::time::timeout_at(deadline, self.seal_one(database, kind, id)).await {
                Ok(Ok(Attempted::AlreadySealed)) => {}
                Ok(Ok(Attempted::Held)) => progress.retries.push_back((kind, id)),
                Ok(Ok(Attempted::Sealed)) => progress.sealed_of(kind),
                Ok(Err(_)) | Err(_) => {
                    *progress.left.of(kind) = progress.left.of(kind).map(|n| n.saturating_add(1));
                }
            }
        }
    }

    /// Seals one row of `kind`.
    async fn seal_one(
        &self,
        database: &PlatformDatabase,
        kind: Kind,
        id: i64,
    ) -> Result<Attempted, DbError> {
        match kind {
            Kind::Resources => self.seal_resource(database, id).await,
            Kind::OwnDatabases => self.seal_own_database(database, id).await,
        }
    }

    /// Seals the connection of the Resource `id` if it is still in clear,
    /// holding its row by its lock while it is read and written. The row's
    /// lock is waited for at most [`ROW_LOCK_WAIT`]: then the transaction
    /// gives up and the caller comes back to it later.
    async fn seal_resource(
        &self,
        database: &PlatformDatabase,
        id: i64,
    ) -> Result<Attempted, DbError> {
        database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .batch_execute(&format!("SET LOCAL lock_timeout = '{ROW_LOCK_WAIT}'"))
                    .await
                    .map_err(DbError::from)?;
                let found = match transaction
                    .query_opt(
                        "SELECT name, connection FROM resources WHERE id = $1 FOR UPDATE",
                        &[&id],
                    )
                    .await
                {
                    Ok(found) => found,
                    Err(error) => return Err(DbError::from(error)),
                };
                let Some(row) = found else {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(Attempted::AlreadySealed);
                };
                let name: String = row.try_get(0).map_err(DbError::from)?;
                let stored: Vec<u8> = row.try_get(1).map_err(DbError::from)?;
                if seal::is_sealed(&stored) {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(Attempted::AlreadySealed);
                }
                let bound = resource_connection_bound(id, &name);
                let sealed = self
                    .resources
                    .seal_bytes(stored, &bound)
                    .ok_or_else(no_randomness)?;
                transaction
                    .execute(
                        "UPDATE resources SET connection = $2 WHERE id = $1",
                        &[&id, &sealed],
                    )
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Attempted::Sealed)
            })
            .await
            .or_else(|error| {
                if error.code() == Some(&SqlState::LOCK_NOT_AVAILABLE) {
                    Ok(Attempted::Held)
                } else {
                    Err(error)
                }
            })
    }

    /// Seals the role passwords of the own database of `application_id`
    /// that are still in clear, holding its row by its lock as a Resource's
    /// is held.
    async fn seal_own_database(
        &self,
        database: &PlatformDatabase,
        application_id: i64,
    ) -> Result<Attempted, DbError> {
        database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .batch_execute(&format!("SET LOCAL lock_timeout = '{ROW_LOCK_WAIT}'"))
                    .await
                    .map_err(DbError::from)?;
                let found = match transaction
                    .query_opt(
                        "SELECT platform_password, runtime_password, owner_password
                           FROM own_databases WHERE application_id = $1 FOR UPDATE",
                        &[&application_id],
                    )
                    .await
                {
                    Ok(found) => found,
                    Err(error) => return Err(DbError::from(error)),
                };
                let Some(row) = found else {
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(Attempted::AlreadySealed);
                };
                let mut sealed_any = false;
                for (index, which) in [
                    (0_usize, Password::Platform),
                    (1, Password::Runtime),
                    (2, Password::Owner),
                ] {
                    let stored: Option<String> = row.try_get(index).map_err(DbError::from)?;
                    let Some(stored) = stored else {
                        continue;
                    };
                    if is_sealed_own_password(&stored) {
                        continue;
                    }
                    let sealed = seal_own_password(self, application_id, which, &stored)
                        .ok_or_else(no_randomness)?;
                    transaction
                        .execute(
                            &format!(
                                "UPDATE own_databases SET {} = $2 WHERE application_id = $1",
                                which.column()
                            ),
                            &[&application_id, &sealed],
                        )
                        .await
                        .map_err(DbError::from)?;
                    sealed_any = true;
                }
                transaction.commit().await.map_err(DbError::from)?;
                Ok(if sealed_any {
                    Attempted::Sealed
                } else {
                    Attempted::AlreadySealed
                })
            })
            .await
            .or_else(|error| {
                if error.code() == Some(&SqlState::LOCK_NOT_AVAILABLE) {
                    Ok(Attempted::Held)
                } else {
                    Err(error)
                }
            })
    }
}

/// What one attempt at a row came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attempted {
    /// It was in clear, and is sealed now.
    Sealed,
    /// It was sealed already.
    AlreadySealed,
    /// Another session held its row: not waited for; try again later.
    Held,
}

/// How many rows the sealing sealed, and how many were left in clear.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sealed {
    pub resources: u64,
    pub own_databases: u64,
}

/// How many rows of each kind are still in clear; `None` when the sealing
/// could not find or count its kind within [`SEALING_BOUND`].
#[derive(Clone, Copy, Debug, Default)]
struct Left {
    resources: Option<u64>,
    own_databases: Option<u64>,
}

impl Left {
    fn of(&mut self, kind: Kind) -> &mut Option<u64> {
        match kind {
            Kind::Resources => &mut self.resources,
            Kind::OwnDatabases => &mut self.own_databases,
        }
    }
}

/// The two kinds of row the sealing takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Resources,
    OwnDatabases,
}

/// What the sealing has done so far, and the rows it must come back to.
#[derive(Default)]
struct Progress {
    sealed: Sealed,
    left: Left,
    retries: VecDeque<(Kind, i64)>,
}

impl Progress {
    fn sealed_of(&mut self, kind: Kind) {
        match kind {
            Kind::Resources => self.sealed.resources = self.sealed.resources.saturating_add(1),
            Kind::OwnDatabases => {
                self.sealed.own_databases = self.sealed.own_databases.saturating_add(1);
            }
        }
    }
}

/// The operating system gave no randomness, so nothing can be sealed: a
/// value that would not serve anyway is left as it was.
fn no_randomness() -> DbError {
    DbError::unexpected("sealing it: the operating system gave no randomness")
}

/// The records of a sealing (blackbox/CONTRACT.md, "Resources and Grants in
/// full: At start"): `credentials-sealed` and the note of old copies when it
/// sealed any row, and `credentials-sealing-incomplete` when any row is
/// still in clear, with `null` for a kind whose rows could not be found or
/// counted in time; nothing when there was nothing to seal and nothing left.
fn finish(sealed: Sealed, left: Left) -> Sealed {
    if sealed.resources > 0 || sealed.own_databases > 0 {
        log::record(
            "credentials-sealed",
            json!({
                "resources": sealed.resources,
                "own_databases": sealed.own_databases,
            }),
        );
        log::record(
            "credentials-sealed-old-copies",
            json!({
                "message": "earlier copies of the clear values remain in backups and in the database server's old files, where this program cannot reach them; the logins of shared databases should be changed at those databases and replaced here (replacing a Resource's logins comes with the next stage of this ticket)",
            }),
        );
    }
    let incomplete = left.resources.is_none()
        || left.own_databases.is_none()
        || left.resources.is_some_and(|n| n > 0)
        || left.own_databases.is_some_and(|n| n > 0);
    if incomplete {
        log::record(
            "credentials-sealing-incomplete",
            json!({
                "resources": left.resources,
                "own_databases": left.own_databases,
            }),
        );
    }
    sealed
}

/// The ids of the database Resources, oldest first.
async fn resource_ids(database: &PlatformDatabase) -> Result<Vec<i64>, DbError> {
    let rows = database
        .pool()
        .run(async |client: &mut Client| {
            client
                .query(
                    "SELECT id FROM resources WHERE kind = 'postgres' ORDER BY id",
                    &[],
                )
                .await
                .map_err(DbError::from)
        })
        .await?;
    rows.iter()
        .map(|row| row.try_get::<_, i64>(0).map_err(DbError::from))
        .collect()
}

/// The Application ids of the own databases, oldest first.
async fn own_database_ids(database: &PlatformDatabase) -> Result<Vec<i64>, DbError> {
    let rows = database
        .pool()
        .run(async |client: &mut Client| {
            client
                .query(
                    "SELECT application_id FROM own_databases ORDER BY application_id",
                    &[],
                )
                .await
                .map_err(DbError::from)
        })
        .await?;
    rows.iter()
        .map(|row| row.try_get::<_, i64>(0).map_err(DbError::from))
        .collect()
}

/// Which password of an own database this is.
#[derive(Clone, Copy, Debug)]
pub enum Password {
    /// The platform's own role's.
    Platform,
    /// The runtime role's, which Application code runs as.
    Runtime,
    /// The migration role's, where a row still has one (the form before
    /// ADR 0017).
    Owner,
}

impl Password {
    /// The column of `own_databases` that holds it.
    pub fn column(self) -> &'static str {
        match self {
            Self::Platform => "platform_password",
            Self::Runtime => "runtime_password",
            Self::Owner => "owner_password",
        }
    }

    fn which(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Runtime => "runtime",
            Self::Owner => "owner",
        }
    }
}

/// The associated data of a Resource's connection: the fixed label, its id
/// and its name, hashed. A value sealed for one row does not open in
/// another, nor in a row whose id or name is not the one it was sealed for.
pub fn resource_connection_bound(id: i64, name: &str) -> [u8; 32] {
    digest(&format!(
        "vm: a database Resource's connection\n{id}\n{name}"
    ))
}

/// The associated data of an own database's password: the fixed label, the
/// Application's id and which password it is.
pub fn own_database_password_bound(application_id: i64, which: Password) -> [u8; 32] {
    digest(&format!(
        "vm: an own database's password\n{application_id}\n{}",
        which.which()
    ))
}

/// Whether `text` is the stored form of an own database's password (this
/// ticket's) rather than the clear form of before.
pub fn is_sealed_own_password(text: &str) -> bool {
    text.starts_with(OWN_FORM_PREFIX)
}

/// `text`, the password of `application_id`'s `which` role, sealed for it;
/// `None` when the operating system gives no randomness.
pub fn seal_own_password(
    vault: &Vault,
    application_id: i64,
    which: Password,
    text: &str,
) -> Option<String> {
    let bound = own_database_password_bound(application_id, which);
    let sealed = vault
        .own_database_sealer()
        .seal_bytes(text.as_bytes().to_vec(), &bound)?;
    Some(format!("{OWN_FORM_PREFIX}{}", STANDARD.encode(sealed)))
}

/// The password sealed in `stored` for `application_id`'s `which` role;
/// `None` when it does not open (another key, another row, the clear form
/// of before, damaged).
pub fn open_own_password(
    vault: &Vault,
    application_id: i64,
    which: Password,
    stored: &str,
) -> Option<String> {
    let coded = stored.strip_prefix(OWN_FORM_PREFIX)?;
    let sealed = STANDARD.decode(coded).ok()?;
    let bound = own_database_password_bound(application_id, which);
    let opened = vault.own_database_sealer().open_bytes(&sealed, &bound)?;
    String::from_utf8(opened).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> Vault {
        let key = InstallationKey::random().expect("the operating system gives randomness");
        Vault::new(&key).expect("a key is derived")
    }

    #[test]
    fn a_connection_bound_to_one_row_does_not_open_in_another() {
        let vault = vault();
        let bound = resource_connection_bound(1, "devops");
        let sealed = vault
            .resource_sealer()
            .seal_bytes(b"{}".to_vec(), &bound)
            .expect("the operating system gives randomness");
        assert!(seal::is_sealed(&sealed));
        assert!(
            vault
                .resource_sealer()
                .open_bytes(&sealed, &resource_connection_bound(1, "devops"))
                .is_some()
        );
        for other in [
            resource_connection_bound(2, "devops"),
            resource_connection_bound(1, "ledger"),
        ] {
            assert!(
                vault
                    .resource_sealer()
                    .open_bytes(&sealed, &other)
                    .is_none()
            );
        }
    }

    #[test]
    fn an_own_database_password_is_told_from_the_clear_form_and_opens_only_in_its_row() {
        let vault = vault();
        let stored = seal_own_password(&vault, 7, Password::Runtime, "0123456789abcdef")
            .expect("the operating system gives randomness");
        assert!(is_sealed_own_password(&stored));
        assert!(!is_sealed_own_password("0123456789abcdef"));
        assert_eq!(
            open_own_password(&vault, 7, Password::Runtime, &stored).as_deref(),
            Some("0123456789abcdef")
        );
        assert!(open_own_password(&vault, 8, Password::Runtime, &stored).is_none());
        assert!(open_own_password(&vault, 7, Password::Platform, &stored).is_none());
        assert!(open_own_password(&vault, 7, Password::Runtime, "0123456789abcdef").is_none());
    }

    #[test]
    fn another_key_does_not_open_what_was_sealed() {
        let first = vault();
        let second = vault();
        let bound = own_database_password_bound(3, Password::Platform);
        let sealed = first
            .own_database_sealer()
            .seal_bytes(b"secret".to_vec(), &bound)
            .expect("the operating system gives randomness");
        assert!(
            second
                .own_database_sealer()
                .open_bytes(&sealed, &bound)
                .is_none()
        );
    }
}
