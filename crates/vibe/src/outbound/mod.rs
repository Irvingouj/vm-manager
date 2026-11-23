//! One list of the hosts Applications may call, held in the program's
//! memory and kept in the platform database (ADR 0020; blackbox/CONTRACT.md,
//! "Outbound calls: The list").
//!
//! Interface:
//!
//! - [`Entry`] and [`Entry::parse`]: one entry of the list, or a host that
//!   may be called, read the same way by the API, the tool and (the next
//!   stage) the host deciding a call: lower case, `xn--` for a name outside
//!   ASCII, no `:443`, the address in brackets. [`EntryProblem`] says why a
//!   text is neither.
//! - [`List`]: the entries in memory, behind the synchronous question
//!   [`List::allows`]: no call's decision reads a database or waits for one.
//! - [`Outbound`]: the list as the program holds it: read at start, changed
//!   by each `PUT` and `DELETE` of the API that succeeded, and its rows in
//!   the platform database.
//! - [`PrivateRanges`]: the ranges `vibe serve --outbound-private` opens,
//!   parsed once, asked with [`PrivateRanges::contains`].

mod entry;
mod private;

pub use entry::{Entry, EntryProblem, MAX_FILTER_BYTES, problem_words};
pub use private::PrivateRanges;

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_postgres::Client;

use crate::platform_db::{DbError, PlatformDatabase, rfc3339};

/// The most entries the list holds, enforced where it is written.
pub const MOST: usize = 1000;

/// The longest an answer of the API to a write of the list may take: the
/// administrators' requests' bound (blackbox/CONTRACT.md, "The list").
pub const WRITE_TIME: Duration = Duration::from_secs(10);

/// The longest one read-back of the table after a write whose outcome is
/// unknown may take.
pub const RELOAD_TIME: Duration = Duration::from_secs(5);

/// The pause between two read-backs while the platform database cannot be
/// read.
pub const RELOAD_PAUSE: Duration = Duration::from_millis(200);

/// One entry as the list holds it, and who added it and when.
#[derive(Clone, Debug)]
pub struct Kept {
    entry: Entry,
    added_by: String,
    added_at: String,
}

impl Kept {
    /// The entry as it is written: [`Entry::text`].
    pub fn host(&self) -> &str {
        self.entry.text()
    }

    pub fn added_by(&self) -> &str {
        &self.added_by
    }

    /// RFC 3339, as the platform database writes the time.
    pub fn added_at(&self) -> &str {
        &self.added_at
    }

    /// The parsed entry, for a question of what it allows.
    pub fn entry(&self) -> &Entry {
        &self.entry
    }
}

/// The entries in memory, by the byte order of their text.
#[derive(Debug, Default)]
pub struct List {
    entries: BTreeMap<String, Kept>,
}

impl List {
    /// A list of `entries`, with no one who added them (a test's, or a
    /// list whose rows are read elsewhere).
    pub fn of<I: IntoIterator<Item = Entry>>(entries: I) -> Self {
        let mut list = Self::default();
        for entry in entries {
            list.insert(Kept {
                added_by: String::new(),
                added_at: String::new(),
                entry,
            });
        }
        list
    }

    /// Whether `host` on `port` is one of the entries: a name exactly, a
    /// wildcard with one label or more beneath it, or an address exactly.
    /// Answers at once, from memory.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        let Ok(asked) = Entry::at(host, port) else {
            return false;
        };
        self.entries.values().any(|kept| kept.entry.allows(&asked))
    }

    /// Every entry, in the byte order of its text.
    pub fn entries(&self) -> impl Iterator<Item = &Kept> {
        self.entries.values()
    }

    fn insert(&mut self, kept: Kept) {
        // The map is keyed by the text it holds as well: an entry as it
        // would be written finds its own row without being parsed again.
        self.entries.insert(kept.entry.text().to_owned(), kept);
    }

    fn remove(&mut self, text: &str) -> Option<Kept> {
        self.entries.remove(text)
    }
}

/// What adding came to.
#[derive(Debug)]
pub struct Put {
    pub kept: Kept,
    /// Whether the entry was added now (it was not there before).
    pub changed: bool,
}

/// What removing came to.
#[derive(Debug)]
pub struct Removed {
    pub host: String,
    /// Whether the entry was there (and is gone now).
    pub changed: bool,
}

/// Why an entry could not be added.
#[derive(Debug)]
pub enum AddProblem {
    /// The list holds [`MOST`] entries already.
    TooMany,
    /// The platform database could not be written.
    Database(DbError),
}

/// The list as the program holds it.
pub struct Outbound {
    database: Arc<PlatformDatabase>,
    /// One writer at a time: the rows and the memory change in the order
    /// the writes are answered, never one before another. A write holds it
    /// at most [`WRITE_TIME`], await included; a read-back of the table
    /// takes it too, so what it loads cannot be overtaken by a write.
    turn: tokio::sync::Mutex<()>,
    /// Held only for a lookup or an insert, never across an await.
    list: RwLock<List>,
    /// The one read-back of the table, started by the first write whose
    /// outcome is unknown; owned here and ended when this is dropped.
    read_back: std::sync::Mutex<ReadBack>,
}

/// The read-back of the table after a write whose outcome is unknown: the
/// request already made of it, and the task that serves every request.
#[derive(Default)]
struct ReadBack {
    /// A request is waiting, or one will be sent: `try_send` on a full
    /// channel leaves one there, which the task reads once its attempt is
    /// over.
    ask: Option<mpsc::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Outbound {
    /// The task that reads the table back ends with what owns it (rule 6:
    /// every spawned task has an owner that ends it).
    fn drop(&mut self) {
        let read_back = self
            .read_back
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(task) = read_back.task.take() {
            task.abort();
        }
    }
}

impl Outbound {
    /// Reads every entry from the platform database into memory. Its
    /// answer is what calls are decided against from now on. A row that is
    /// not an entry is not allowed and not answered; the program starts all
    /// the same, and its log says that a row could not be read, without the
    /// row's text (one record for the load when there is at least one).
    pub async fn load(database: Arc<PlatformDatabase>) -> Result<Self, DbError> {
        let rows = read_rows(&database).await?;
        let (list, unreadable) = list_from(rows);
        log_unreadable(unreadable);
        Ok(Self {
            database,
            turn: tokio::sync::Mutex::new(()),
            list: RwLock::new(list),
            read_back: std::sync::Mutex::new(ReadBack::default()),
        })
    }

    /// Whether `host` on `port` may be called, from memory: no database is
    /// read and none is waited for (blackbox/CONTRACT.md, "The list").
    pub fn allows(&self, host: &str, port: u16) -> bool {
        self.list
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .allows(host, port)
    }

    /// Every entry, in the byte order of its text, as the API answers them.
    pub fn entries(&self) -> Vec<Kept> {
        // Cloned to answer with: the page or the answer is written after
        // the lock is let go, and it needs what it shows.
        self.list
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .entries()
            .cloned()
            .collect()
    }

    /// Adds `entry`, by `by`: in the platform database and then in memory,
    /// before this returns. An entry that is there already is answered
    /// with who added it first and when, `changed` false. The [`MOST`]th
    /// entry and every one after it is [`AddProblem::TooMany`].
    ///
    /// A write whose outcome is not known — an error, or the 10 seconds
    /// passing with the statement possibly sent — takes the entry out of
    /// memory before this answers, and has the table read back: what the
    /// program holds is never more than the table holds.
    pub async fn add(self: &Arc<Self>, entry: &Entry, by: &str) -> Result<Put, AddProblem> {
        // The statements below borrow the text while the transaction runs:
        // the entry itself is lent for this call only.
        let text = entry.text();
        let written = tokio::time::timeout(WRITE_TIME, async {
            let _turn = self.turn.lock().await;
            match self.insert_row(text, by).await {
                Ok(Found::There { added_by, added_at }) => {
                    let kept = Kept {
                        // Memory keeps the entry after this call returns,
                        // so it holds its own; the answer holds the same.
                        entry: entry.clone(),
                        added_by,
                        added_at,
                    };
                    self.remember(kept.clone());
                    Ok(Put {
                        kept,
                        changed: false,
                    })
                }
                Ok(Found::Inserted { added_by, added_at }) => {
                    let kept = Kept {
                        entry: entry.clone(),
                        added_by,
                        added_at,
                    };
                    self.remember(kept.clone());
                    Ok(Put {
                        kept,
                        changed: true,
                    })
                }
                Ok(Found::Full) => Err(AddProblem::TooMany),
                Err(error) => {
                    // The row may have been written: not allowed from now
                    // on, until the table has been read back.
                    self.forget(text);
                    Err(AddProblem::Database(error))
                }
            }
        })
        .await;
        match written {
            Ok(answered) => {
                if matches!(answered, Err(AddProblem::Database(_))) {
                    self.read_back_later();
                }
                answered
            }
            Err(_) => {
                // The future was dropped, its turn with it: whether the
                // statement was sent, the entry is not allowed now.
                {
                    let _turn = self.turn.lock().await;
                    self.forget(text);
                }
                self.read_back_later();
                Err(AddProblem::Database(DbError::timed_out(
                    "adding a host to the outbound list",
                    WRITE_TIME,
                )))
            }
        }
    }

    /// Removes `entry`: from memory before the statement is sent, then from
    /// the platform database, before this returns; `changed` false when it
    /// was not there.
    ///
    /// Every ending whose outcome is not known leaves the entry out of
    /// memory and has the table read back, whichever way the `DELETE` went.
    pub async fn remove(self: &Arc<Self>, entry: &Entry) -> Result<Removed, DbError> {
        let text = entry.text();
        let written = tokio::time::timeout(WRITE_TIME, async {
            let _turn = self.turn.lock().await;
            // Out before the statement: a removal in doubt never allows it.
            self.forget(text);
            match self.delete_row(text).await {
                Ok(removed) => Ok(Removed {
                    // The answer holds the host after this call returns.
                    host: text.to_owned(),
                    changed: removed > 0,
                }),
                // The delete may have been made: the read-back settles it.
                Err(error) => Err(error),
            }
        })
        .await;
        match written {
            Ok(answered) => {
                if answered.is_err() {
                    self.read_back_later();
                }
                answered
            }
            Err(_) => {
                // The future may have been dropped before the entry was
                // taken out: take it out now, then read the table back.
                {
                    let _turn = self.turn.lock().await;
                    self.forget(text);
                }
                self.read_back_later();
                Err(DbError::timed_out(
                    "removing a host from the outbound list",
                    WRITE_TIME,
                ))
            }
        }
    }

    /// The entry `text` out of memory. The caller holds the turn.
    fn forget(&self, text: &str) {
        self.list
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(text);
    }

    /// `kept` in memory. The caller holds the turn.
    fn remember(&self, kept: Kept) {
        self.list
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(kept);
    }

    /// Asks the one read-back task to read the whole table into memory,
    /// starting it the first time. Asked again while it runs, the task reads
    /// once more after the attempt under way: a call to this made after an
    /// entry was forgotten under the turn is served by a read that starts
    /// after that (the read-back takes the turn too).
    fn read_back_later(self: &Arc<Self>) {
        let mut read_back = self
            .read_back
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if read_back.task.is_none() {
            let (ask, mut asked) = mpsc::channel(1);
            let this = Arc::downgrade(self);
            read_back.task = Some(tokio::spawn(async move {
                while asked.recv().await.is_some() {
                    loop {
                        let Some(outbound) = this.upgrade() else {
                            return;
                        };
                        let read = outbound.reload().await.is_ok();
                        // Neither the task nor a pause holds the list.
                        drop(outbound);
                        if read {
                            break;
                        }
                        tokio::time::sleep(RELOAD_PAUSE).await;
                    }
                }
            }));
            read_back.ask = Some(ask);
        }
        if let Some(ask) = &read_back.ask {
            // A full channel already holds a request the task reads after
            // its attempt under way, which began after the last forget.
            match ask.try_send(()) {
                Ok(()) => {}
                Err(_) => {
                    // A request is waiting already: it is served next.
                }
            }
        }
    }

    /// One attempt at reading the whole table into memory, under the turn
    /// and within [`RELOAD_TIME`]: the table as it is with no write in
    /// flight (the lock waits for one), and exactly what memory then holds.
    async fn reload(&self) -> Result<(), DbError> {
        let attempt = tokio::time::timeout(RELOAD_TIME, async {
            let _turn = self.turn.lock().await;
            let rows = read_rows(&self.database).await?;
            let (list, unreadable) = list_from(rows);
            log_unreadable(unreadable);
            *self.list.write().unwrap_or_else(PoisonError::into_inner) = list;
            Ok::<(), DbError>(())
        })
        .await;
        match attempt {
            Ok(read) => read,
            Err(_) => Err(DbError::timed_out(
                "reading the outbound list back",
                RELOAD_TIME,
            )),
        }
    }

    /// One row added to the table, or the row already there, in one
    /// transaction whose table lock keeps the count from being passed by a
    /// second writer.
    async fn insert_row(&self, text: &str, by: &str) -> Result<Found, DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .batch_execute(&table_lock(WRITE_TIME))
                    .await
                    .map_err(DbError::from)?;
                let found = transaction
                    .query_opt(
                        concat!(
                            "SELECT added_by, ",
                            rfc3339!("added_at"),
                            " FROM outbound_hosts WHERE host = $1"
                        ),
                        &[&text],
                    )
                    .await
                    .map_err(DbError::from)?;
                if let Some(row) = found {
                    let added_by: String = get(&row, 0)?;
                    let added_at: String = get(&row, 1)?;
                    transaction.commit().await.map_err(DbError::from)?;
                    return Ok(Found::There { added_by, added_at });
                }
                let row = transaction
                    .query_one("SELECT count(*) FROM outbound_hosts", &[])
                    .await
                    .map_err(DbError::from)?;
                let count: i64 = get(&row, 0)?;
                if usize::try_from(count).unwrap_or(MOST) >= MOST {
                    transaction.rollback().await.map_err(DbError::from)?;
                    return Ok(Found::Full);
                }
                let row = transaction
                    .query_one(
                        concat!(
                            "INSERT INTO outbound_hosts (host, added_by) VALUES ($1, $2)
                             RETURNING added_by, ",
                            rfc3339!("added_at")
                        ),
                        &[&text, &by],
                    )
                    .await
                    .map_err(DbError::from)?;
                let added_by: String = get(&row, 0)?;
                let added_at: String = get(&row, 1)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(Found::Inserted { added_by, added_at })
            })
            .await
    }

    /// The row `text` deleted, and how many were.
    async fn delete_row(&self, text: &str) -> Result<u64, DbError> {
        self.database
            .pool()
            .run(async |client: &mut Client| {
                // A transaction that takes the table's lock first, and not
                // one statement on its own: a removal whose bytes reach the
                // database late (after this gave up, and after the table
                // was read back) must not take effect then. Cut before the
                // lock was taken, the `DELETE` is never sent; cut after,
                // the lock is held until the transaction ends, and the
                // read-back waits for it.
                let transaction = client.transaction().await.map_err(DbError::from)?;
                transaction
                    .batch_execute(&table_lock(WRITE_TIME))
                    .await
                    .map_err(DbError::from)?;
                let removed = transaction
                    .execute("DELETE FROM outbound_hosts WHERE host = $1", &[&text])
                    .await
                    .map_err(DbError::from)?;
                transaction.commit().await.map_err(DbError::from)?;
                Ok(removed)
            })
            .await
    }
}

/// What the write of an entry found in the platform database.
enum Found {
    There { added_by: String, added_at: String },
    Inserted { added_by: String, added_at: String },
    Full,
}

/// The rows of the table as `(host, added_by, added_at)`, at most [`MOST`]
/// of them in the byte order of `host`, read under the table's `EXCLUSIVE`
/// lock: a write in flight (its table or row lock, held until its commit)
/// finishes before the read, so what is read is the table after it, never
/// before it. That is what makes a read-back after a lost write sound.
/// The statements that open every transaction on the table: the server's
/// own limit on the wait for the lock and on each statement (`limit`, so a
/// transaction given up here is given up there too and no backend is left
/// waiting), then the table's lock. `EXCLUSIVE` lets plain readings through
/// and makes every transaction of this module wait for the one before it.
fn table_lock(limit: Duration) -> String {
    format!(
        "{}; LOCK TABLE outbound_hosts IN EXCLUSIVE MODE",
        crate::registry::statement_timeout(limit)
    )
}

async fn read_rows(database: &PlatformDatabase) -> Result<Vec<(String, String, String)>, DbError> {
    let limit = i64::try_from(MOST).unwrap_or(i64::MAX);
    database
        .pool()
        .run(async |client: &mut Client| {
            let transaction = client.transaction().await.map_err(DbError::from)?;
            // `EXCLUSIVE` conflicts with every write's lock and with no
            // plain reading's, so only writes wait for it.
            transaction
                .batch_execute(&table_lock(RELOAD_TIME))
                .await
                .map_err(DbError::from)?;
            let rows = transaction
                .query(
                    concat!(
                        "SELECT host, added_by, ",
                        rfc3339!("added_at"),
                        " FROM outbound_hosts ORDER BY host COLLATE \"C\" LIMIT $1"
                    ),
                    &[&limit],
                )
                .await
                .map_err(DbError::from)?;
            transaction.commit().await.map_err(DbError::from)?;
            let mut read = Vec::with_capacity(rows.len());
            for row in &rows {
                read.push((
                    get::<String>(row, 0)?,
                    get::<String>(row, 1)?,
                    get::<String>(row, 2)?,
                ));
            }
            Ok(read)
        })
        .await
}

/// The list of these rows (`host`, `added_by`, `added_at`), and how many of
/// them are not an entry: those are not allowed and not answered.
fn list_from<I: IntoIterator<Item = (String, String, String)>>(rows: I) -> (List, usize) {
    let mut list = List::default();
    let mut unreadable = 0;
    for (host, added_by, added_at) in rows {
        match Entry::parse(&host) {
            Ok(entry) => list.insert(Kept {
                entry,
                added_by,
                added_at,
            }),
            Err(_) => unreadable += 1,
        }
    }
    (list, unreadable)
}

/// The program's log, once for a load of the table that held a row it cannot
/// read as an entry: the event only, never the row's text. A table written by
/// this program can hold at most [`MOST`] rows, so one record for the load
/// stands for all of them.
fn log_unreadable(unreadable: usize) {
    if unreadable > 0 {
        crate::log::record("outbound-host-unreadable", serde_json::json!({}));
    }
}

fn get<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a tokio_postgres::Row,
    index: usize,
) -> Result<T, DbError> {
    row.try_get(index).map_err(DbError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(texts: &[&str]) -> List {
        List::of(
            texts
                .iter()
                .map(|t| Entry::parse(t).unwrap_or_else(|e| panic!("{t:?}: {}", e.words()))),
        )
    }

    #[test]
    fn a_name_allows_itself_on_its_port_and_nothing_else() {
        let list = entries(&["api.example.com", "api.example.com:8443"]);
        assert!(list.allows("api.example.com", 443));
        assert!(list.allows("API.EXAMPLE.COM", 443));
        assert!(list.allows("api.example.com", 8443));
        assert!(!list.allows("api.example.com", 8444));
        assert!(!list.allows("other.example.com", 443));
        assert!(!list.allows("a.api.example.com", 443));
        assert!(!list.allows("example.com", 443));
    }

    #[test]
    fn a_wildcard_allows_one_label_or_more_beneath_it_and_not_its_base() {
        let list = entries(&["*.example.com", "*.ported.invalid:8443"]);
        assert!(list.allows("a.example.com", 443));
        assert!(list.allows("a.b.example.com", 443));
        assert!(!list.allows("example.com", 443));
        assert!(!list.allows("aexample.com", 443));
        assert!(!list.allows("a.example.com", 8443));
        assert!(list.allows("x.ported.invalid", 8443));
        assert!(!list.allows("x.ported.invalid", 443));
        assert!(!list.allows("ported.invalid", 8443));
    }

    #[test]
    fn an_address_is_allowed_by_its_address_on_its_port() {
        let list = entries(&["203.0.113.7", "[2001:db8::7]:8443", "[2001:db8::7]"]);
        assert!(list.allows("203.0.113.7", 443));
        assert!(!list.allows("203.0.113.7", 8443));
        assert!(list.allows("[2001:db8::7]", 8443));
        assert!(list.allows("[2001:db8::7]", 443));
        assert!(list.allows("2001:db8::7", 443));
        assert!(!list.allows("[2001:db8::8]", 443));
        // A name that only looks like an address is not one.
        assert!(!list.allows("0xcb.0.113.7", 443));
    }

    #[test]
    fn what_is_not_a_host_allows_nothing() {
        let list = entries(&["api.example.com"]);
        for host in ["", "api.example.com:443", "api.example.com/x", "a b"] {
            assert!(!list.allows(host, 443), "{host:?}");
        }
    }

    #[test]
    fn the_entries_are_in_the_byte_order_of_their_text() {
        let list = entries(&["c.example.com", "a.example.com", "*.example.com"]);
        let hosts: Vec<&str> = list.entries().map(Kept::host).collect();
        assert_eq!(hosts, ["*.example.com", "a.example.com", "c.example.com"]);
    }

    #[test]
    fn a_row_that_is_not_an_entry_is_not_held_and_the_rest_are() {
        let rows = vec![
            (
                "api.example.com".to_owned(),
                "ada".to_owned(),
                "2026-11-23T09:00:00.000000Z".to_owned(),
            ),
            // A row another hand, or a program with other rules, wrote.
            ("a b".to_owned(), "bob".to_owned(), "t".to_owned()),
            // A name a resolver may read as an address: no entry now.
            ("127.1".to_owned(), "cy".to_owned(), "t".to_owned()),
            ("*.example.org".to_owned(), "dee".to_owned(), "t".to_owned()),
        ];
        let (list, unreadable) = list_from(rows);
        assert_eq!(unreadable, 2, "the two rows that are not entries");
        let hosts: Vec<&str> = list.entries().map(Kept::host).collect();
        assert_eq!(hosts, ["*.example.org", "api.example.com"]);
        assert!(!list.allows("127.1", 443));
        assert!(!list.allows("a b", 443));
    }
}
